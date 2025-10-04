import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

type SessionStatus = {
  phase: string;
  detail: string;
};

export default function App() {
  const [status, setStatus] = useState<SessionStatus>({
    phase: "Idle",
    detail: "Core service unavailable",
  });

  useEffect(() => {
    invoke<SessionStatus>("session_status")
      .then(setStatus)
      .catch(() =>
        setStatus({ phase: "Unknown", detail: "Core service unreachable" })
      );
  }, []);

  return (
    <main className="app-shell">
      <header className="hero">
        <h1>Flowwisper Fn</h1>
        <p>桌面端壳层脚手架 - 准备连接本地核心服务。</p>
      </header>

      <section className="status">
        <h2>会话状态</h2>
        <div className="status-card">
          <span className="label">Phase:</span>
          <span className="value">{status.phase}</span>
        </div>
        <div className="status-card">
          <span className="label">Detail:</span>
          <span className="value">{status.detail}</span>
        </div>
      </section>

      <section className="next-steps">
        <h2>下一步开发指引</h2>
        <ol>
          <li>在 <code>src-tauri/src/main.rs</code> 中实现热键、音频捕获桥接。</li>
          <li>在 <code>src</code> 目录下补充实时波形、会话 HUD、设置面板等 UI。</li>
          <li>通过 Tauri 命令与 <code>core</code> 服务通信，实现状态同步。</li>
        </ol>
      </section>
    </main>
  );
}
