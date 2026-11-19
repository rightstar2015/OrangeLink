const status = document.getElementById("status");
const code = document.getElementById("code");

function show(text, cls) {
  status.textContent = text;
  status.className = cls;
}

async function check() {
  const { port, token } = await chrome.storage.local.get(["port", "token"]);
  if (!port || !token) return show("尚未配對：請貼上 OrangeLink 設定中的配對碼。", "bad");
  code.value = `${port}-${token}`;
  try {
    const res = await fetch(`http://127.0.0.1:${port}/ping`, { headers: { "X-OrangeLink-Token": token } });
    if (res.status === 401) return show("配對碼不正確，請重新複製。", "bad");
    if (!res.ok) return show(`OrangeLink 回應錯誤（${res.status}）。`, "bad");
    const info = await res.json();
    show(`已連線 OrangeLink ${info.version}`, "ok");
    chrome.runtime.sendMessage({ type: "refresh-menus" }).catch(() => {});
  } catch {
    show("連不到 OrangeLink：請確認程式已開啟，並在設定中啟用瀏覽器擴充功能。", "bad");
  }
}

document.getElementById("save").addEventListener("click", async () => {
  const match = code.value.trim().match(/^(\d{4,5})-([0-9a-f]{32})$/i);
  if (!match) return show("配對碼格式不正確，請從 OrangeLink 設定重新複製。", "bad");
  await chrome.storage.local.set({ port: Number(match[1]), token: match[2] });
  check();
});

document.getElementById("yt").addEventListener("click", async () => {
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  await chrome.runtime.sendMessage({ type: "yt-page", url: tab?.url });
  window.close();
});

check();
