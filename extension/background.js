// Right-click menu: fetches the file with the browser's own session (so logged-in
// and hotlink-protected files work) and hands it to OrangeLink on 127.0.0.1.

chrome.runtime.onInstalled.addListener(() => {
  chrome.alarms.create("menus", { periodInMinutes: 1 });
  refreshMenus(true);
});
chrome.runtime.onStartup.addListener(() => refreshMenus());
chrome.alarms.onAlarm.addListener(() => refreshMenus());

const MAX_FOLDERS = 100;
let menuWork = Promise.resolve();

// The folder list lives in OrangeLink, so the menu follows it; while OrangeLink is
// closed the last known list stays (choosing one then reports that it can't connect).
function refreshMenus(force) {
  menuWork = menuWork.then(async () => {
    const { menuFolders } = await chrome.storage.local.get("menuFolders");
    const folders = await fetchFolders();
    const changed = folders && JSON.stringify(folders) !== JSON.stringify(menuFolders);
    if (!force && !changed) return;
    const list = folders || menuFolders || [];
    await chrome.storage.local.set({ menuFolders: list });
    await buildMenus(list);
  }).catch(() => {});
  return menuWork;
}

async function fetchFolders() {
  try {
    const { port, token } = await server();
    const res = await fetch(`http://127.0.0.1:${port}/folders`, { headers: { "X-OrangeLink-Token": token } });
    return res.ok ? (await res.json()).folders : null;
  } catch {
    return null;
  }
}

async function buildMenus(folders) {
  await chrome.contextMenus.removeAll();
  const contexts = ["image", "video", "audio", "link"];
  chrome.contextMenus.create({ id: "add", title: "加入 OrangeLink", contexts });
  if (folders.length) {
    chrome.contextMenus.create({ id: "add-to", title: "加入 OrangeLink 資料夾", contexts });
    for (const f of folders.slice(0, MAX_FOLDERS)) {
      // "&" marks a keyboard shortcut in menu titles.
      const title = "　".repeat(f.depth) + f.name.replace(/&/g, "&&");
      chrome.contextMenus.create({ id: `folder:${f.id}`, parentId: "add-to", title, contexts });
    }
  }
  chrome.contextMenus.create({ id: "yt-page", title: "用 yt-dlp 下載此頁面", contexts: ["page", "video", "audio"] });
  chrome.contextMenus.create({ id: "yt-link", title: "用 yt-dlp 下載此連結", contexts: ["link"] });
}

chrome.contextMenus.onClicked.addListener((info, tab) => {
  const id = String(info.menuItemId);
  const work =
    id === "add" ? addFile(info, tab)
    : id.startsWith("folder:") ? addFile(info, tab, id.slice("folder:".length))
    : id === "yt-page" ? sendToYtdlp(info.pageUrl)
    : sendToYtdlp(info.linkUrl);
  track(work);
});

chrome.runtime.onMessage.addListener((msg, _sender, respond) => {
  if (msg.type === "yt-page") {
    track(sendToYtdlp(msg.url));
    respond({});
  } else if (msg.type === "refresh-menus") {
    refreshMenus();
    respond({});
  }
});

// Shows activity on the icon and keeps the service worker alive during long transfers.
async function track(work) {
  chrome.action.setBadgeText({ text: "…" });
  chrome.action.setBadgeBackgroundColor({ color: "#e0701a" });
  const keepAlive = setInterval(() => chrome.runtime.getPlatformInfo(() => {}), 20000);
  try {
    notify("已送到 OrangeLink", await work);
  } catch (e) {
    notify("無法加入 OrangeLink", e.message);
  } finally {
    clearInterval(keepAlive);
    chrome.action.setBadgeText({ text: "" });
    refreshMenus();
  }
}

function notify(title, message) {
  chrome.notifications.create({ type: "basic", iconUrl: "icon128.png", title, message: message || "" });
}

async function server() {
  const { port, token } = await chrome.storage.local.get(["port", "token"]);
  if (!port || !token) throw new Error("尚未配對：請點瀏覽器工具列的 OrangeLink 圖示，貼上配對碼。");
  return { port, token };
}

async function post(path, params, body) {
  const { port, token } = await server();
  const query = new URLSearchParams(params).toString().replace(/\+/g, "%20");
  let res;
  try {
    res = await fetch(`http://127.0.0.1:${port}${path}?${query}`, {
      method: "POST",
      headers: { "X-OrangeLink-Token": token },
      body,
    });
  } catch {
    throw new Error("連不到 OrangeLink：請確認程式已開啟，並在設定中啟用瀏覽器擴充功能。");
  }
  if (!res.ok) {
    const data = await res.json().catch(() => ({}));
    throw new Error(data.error || `OrangeLink 回應錯誤（${res.status}）`);
  }
}

async function sendToYtdlp(url) {
  if (!/^https?:/i.test(url || "")) throw new Error("這個頁面沒有可下載的網址。");
  await post("/ytdlp", { url });
  return "已交給 yt-dlp，下載完成後會自動匯入。";
}

async function addFile(info, tab, folder) {
  const src = info.srcUrl || info.linkUrl;
  if (!src) throw new Error("找不到可下載的網址。");
  const pageUrl = info.pageUrl || tab?.url || "";
  let blob, name;
  if (src.startsWith("blob:")) {
    // Streaming players build their video from blob: URLs; there is no file to fetch.
    if (info.mediaType !== "image") {
      const sent = await sendToYtdlp(pageUrl);
      return folder ? `${sent}（串流影片不會放入指定的資料夾）` : sent;
    }
    blob = await blobFromPage(tab, info.frameId, src);
    name = pageName(tab);
  } else {
    const res = await fetchWithReferer(src, pageUrl);
    if (!res.ok) throw new Error(`下載失敗（HTTP ${res.status}），網站可能拒絕了這個請求。`);
    const type = res.headers.get("Content-Type") || "";
    // A link to a web page, or a login/error page instead of the media.
    if (/^text\/html/i.test(type)) {
      throw new Error("這個連結是網頁，不是檔案。若是影片頁面，請改用「用 yt-dlp 下載」。");
    }
    await checkSize(res);
    blob = await res.blob();
    name = nameFromDisposition(res.headers.get("Content-Disposition")) || nameFromUrl(src) || pageName(tab);
  }
  // Source address worth keeping: the page, or the file when the page has none.
  const url = (/^https?:/i.test(pageUrl) ? pageUrl : src).slice(0, 2000);
  await post("/file", { name, url: /^https?:/i.test(url) ? url : "", type: blob.type, folder: folder || "" }, blob);
  return name;
}

// Refuses a file over OrangeLink's limit before it is read into memory (when the
// site says how big it is).
async function checkSize(res) {
  const size = Number(res.headers.get("Content-Length"));
  if (!size) return;
  const { port, token } = await server();
  const ping = await fetch(`http://127.0.0.1:${port}/ping`, { headers: { "X-OrangeLink-Token": token } }).catch(() => null);
  const max = ping?.ok ? (await ping.json()).max_bytes : 0;
  if (max && size > max) {
    res.body?.cancel();
    throw new Error(`檔案有 ${(size / 2 ** 30).toFixed(1)} GB，超過 ${(max / 2 ** 30).toFixed(1)} GB 的上限，可在 OrangeLink 設定調整。`);
  }
}

// Many sites refuse image requests without a Referer from their own pages. The
// rule applies only to this extension's own requests (tabId -1) to that host.
let nextRule = 1;
async function fetchWithReferer(src, pageUrl) {
  const host = /^https?:/i.test(src) && /^https?:/i.test(pageUrl) ? new URL(src).hostname : null;
  const id = nextRule++;
  if (host) {
    await chrome.declarativeNetRequest.updateSessionRules({
      removeRuleIds: [id],
      addRules: [{
        id,
        action: { type: "modifyHeaders", requestHeaders: [{ header: "Referer", operation: "set", value: pageUrl }] },
        condition: { requestDomains: [host], tabIds: [-1], resourceTypes: ["xmlhttprequest"] },
      }],
    });
  }
  try {
    return await fetch(src, { credentials: "include" });
  } catch {
    throw new Error("無法下載這個檔案，網站可能阻擋了連線。");
  } finally {
    if (host) await chrome.declarativeNetRequest.updateSessionRules({ removeRuleIds: [id] });
  }
}

// blob: URLs only resolve inside the page that created them.
async function blobFromPage(tab, frameId, src) {
  const target = { tabId: tab.id };
  if (frameId) target.frameIds = [frameId];
  const [result] = await chrome.scripting.executeScript({
    target,
    func: async (url) => {
      const blob = await (await fetch(url)).blob();
      return await new Promise((done) => {
        const reader = new FileReader();
        reader.onload = () => done(reader.result);
        reader.readAsDataURL(blob);
      });
    },
    args: [src],
  });
  if (!result?.result) throw new Error("無法從頁面取得這張圖片。");
  return await (await fetch(result.result)).blob();
}

function nameFromDisposition(header) {
  if (!header) return "";
  const star = header.match(/filename\*\s*=\s*[^']*'[^']*'([^;]+)/i);
  if (star) {
    try {
      return decodeURIComponent(star[1].trim());
    } catch {}
  }
  const plain = header.match(/filename\s*=\s*"?([^";]+)"?/i);
  return plain ? plain[1].trim() : "";
}

function nameFromUrl(src) {
  try {
    const url = new URL(src);
    if (!/^https?:$/.test(url.protocol)) return "";
    const last = url.pathname.split("/").filter(Boolean).pop() || "";
    return decodeURIComponent(last);
  } catch {
    return "";
  }
}

function pageName(tab) {
  return (tab?.title || "download").slice(0, 80);
}
