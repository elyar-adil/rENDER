"""Built-in standards-first new-tab page."""

HOME_URL = 'about:home'

HOME_HTML = r'''<!doctype html>
<html lang="zh-CN">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <meta name="color-scheme" content="dark">
  <title>rENDER 主页</title>
  <style>
    :root {
      color-scheme: dark;
      --canvas: #080b12;
      --surface: rgba(20, 25, 38, 0.76);
      --surface-strong: #171d2b;
      --surface-hover: #1d2536;
      --line: rgba(255, 255, 255, 0.09);
      --line-strong: rgba(255, 255, 255, 0.16);
      --text: #f5f7ff;
      --muted: #9ba6ba;
      --subtle: #707b90;
      --accent: #7c8cff;
      --accent-strong: #9a7cff;
      --success: #5ee0a0;
      --radius-xl: 28px;
      --radius-lg: 20px;
      --radius-md: 14px;
      --shadow: 0 24px 80px rgba(0, 0, 0, 0.34);
    }

    * { box-sizing: border-box; }

    html, body {
      margin: 0;
      min-height: 100%;
    }

    body {
      min-height: 100dvh;
      color: var(--text);
      background-color: var(--canvas);
      background-image: linear-gradient(180deg, #11172a 0%, #080b12 48%, #080b12 100%);
      font-family: "Segoe UI Variable", "Microsoft YaHei UI", "Microsoft YaHei", sans-serif;
      font-size: 16px;
      line-height: 1.5;
    }

    a { color: inherit; text-decoration: none; }

    .ambient {
      position: fixed;
      top: -250px;
      left: 50%;
      width: 760px;
      height: 560px;
      margin-left: -380px;
      border-radius: 50%;
      background-image: radial-gradient(circle, rgba(111, 126, 255, 0.26) 0%, rgba(111, 126, 255, 0) 70%);
      filter: blur(10px);
      pointer-events: none;
    }

    .topbar {
      position: relative;
      display: flex;
      align-items: center;
      justify-content: space-between;
      width: 92%;
      max-width: 1180px;
      margin: 0 auto;
      padding: 24px 0;
    }

    .wordmark {
      display: inline-flex;
      align-items: center;
      gap: 11px;
      font-size: 17px;
      font-weight: 700;
      letter-spacing: -0.02em;
    }

    .wordmark-mark {
      display: inline-flex;
      align-items: center;
      justify-content: center;
      width: 34px;
      height: 34px;
      color: white;
      background-image: linear-gradient(135deg, #7187ff, #9b6dff);
      border-radius: 11px;
      box-shadow: 0 8px 28px rgba(113, 135, 255, 0.34);
      font-size: 17px;
      font-weight: 800;
    }

    .engine-state {
      display: inline-flex;
      align-items: center;
      gap: 9px;
      margin: 0;
      color: var(--muted);
      font-size: 13px;
    }

    .engine-state-dot {
      width: 8px;
      height: 8px;
      background: var(--success);
      border-radius: 50%;
      box-shadow: 0 0 18px rgba(94, 224, 160, 0.66);
    }

    main {
      position: relative;
      width: 92%;
      max-width: 1100px;
      margin: 0 auto;
      padding: 54px 0 70px;
    }

    .hero {
      max-width: 820px;
      margin: 0 auto;
      text-align: center;
    }

    .eyebrow {
      display: inline-block;
      margin: 0 0 22px;
      padding: 7px 13px;
      color: #cbd1ff;
      background: rgba(124, 140, 255, 0.10);
      border: 1px solid rgba(148, 159, 255, 0.20);
      border-radius: 999px;
      font-size: 12px;
      font-weight: 700;
      letter-spacing: 0.13em;
      text-transform: uppercase;
      white-space: nowrap;
    }

    h1 {
      margin: 0;
      font-size: clamp(44px, 6vw, 72px);
      line-height: 1.08;
      letter-spacing: -0.055em;
    }

    h1 span { color: #aab4ff; }

    .hero-copy {
      max-width: 610px;
      margin: 22px auto 0;
      color: var(--muted);
      font-size: clamp(16px, 2vw, 18px);
    }

    .address-guide {
      display: flex;
      align-items: center;
      width: 92%;
      max-width: 700px;
      min-height: 64px;
      margin: 36px auto 0;
      padding: 10px 12px 10px 20px;
      color: var(--muted);
      background: rgba(22, 28, 43, 0.80);
      border: 1px solid var(--line-strong);
      border-radius: 20px;
      box-shadow: var(--shadow);
      backdrop-filter: blur(24px);
      text-align: left;
    }

    .address-guide:hover {
      color: var(--text);
      background: rgba(29, 37, 56, 0.92);
      border-color: rgba(151, 163, 255, 0.44);
    }

    .search-symbol {
      margin-right: 13px;
      color: #b9c1ff;
      font-size: 24px;
      line-height: 1;
    }

    .address-copy { flex: 1; }

    .key {
      display: inline-block;
      min-width: 28px;
      margin-left: 6px;
      padding: 5px 8px;
      color: #c8cfdd;
      background: #252d3e;
      border: 1px solid rgba(255, 255, 255, 0.10);
      border-radius: 8px;
      box-shadow: 0 2px 0 rgba(0, 0, 0, 0.35);
      font-family: inherit;
      font-size: 12px;
      line-height: 1;
      text-align: center;
    }

    .quick-access { margin-top: 76px; }

    .section-heading {
      display: flex;
      align-items: end;
      justify-content: space-between;
      margin-bottom: 18px;
    }

    h2 {
      margin: 0;
      font-size: 20px;
      line-height: 1.2;
      letter-spacing: -0.025em;
    }

    .section-note {
      margin: 0;
      color: var(--subtle);
      font-size: 13px;
    }

    .site-grid {
      display: grid;
      grid-template-columns: repeat(4, 1fr);
      gap: 14px;
    }

    .site-card {
      position: relative;
      display: flex;
      align-items: center;
      min-width: 0;
      min-height: 88px;
      padding: 16px;
      overflow: hidden;
      background: var(--surface);
      border: 1px solid var(--line);
      border-radius: var(--radius-lg);
      backdrop-filter: blur(18px);
      transition: transform 160ms ease, background-color 160ms ease, border-color 160ms ease;
    }

    .site-card:hover {
      background: var(--surface-hover);
      border-color: var(--line-strong);
      transform: translateY(-3px);
    }

    .site-icon {
      display: inline-flex;
      flex: 0 0 48px;
      align-items: center;
      justify-content: center;
      width: 48px;
      height: 48px;
      margin-right: 14px;
      color: white;
      border-radius: 15px;
      font-size: 17px;
      font-weight: 800;
      letter-spacing: -0.04em;
      box-shadow: 0 10px 28px rgba(0, 0, 0, 0.20);
    }

    .tone-blue { background-image: linear-gradient(135deg, #4a8cff, #5367ef); }
    .tone-cyan { background-image: linear-gradient(135deg, #22b8cf, #267de4); }
    .tone-red { background-image: linear-gradient(135deg, #ff6565, #e7435b); }
    .tone-orange { background-image: linear-gradient(135deg, #ff9f43, #f06d3e); }
    .tone-green { background-image: linear-gradient(135deg, #32c88b, #159b75); }
    .tone-purple { background-image: linear-gradient(135deg, #a875ff, #7058e8); }
    .tone-pink { background-image: linear-gradient(135deg, #ff78aa, #df4d86); }
    .tone-slate { background-image: linear-gradient(135deg, #71809b, #46536d); }

    .site-meta {
      min-width: 0;
      flex: 1;
    }

    .site-name {
      display: block;
      overflow: hidden;
      color: var(--text);
      font-size: 16px;
      font-weight: 700;
      line-height: 1.35;
      text-overflow: ellipsis;
      white-space: nowrap;
    }

    .site-host {
      display: block;
      margin-top: 4px;
      overflow: hidden;
      color: var(--subtle);
      font-size: 12px;
      line-height: 1.3;
      text-overflow: ellipsis;
      white-space: nowrap;
    }

    .site-arrow {
      margin-left: 9px;
      color: #616c80;
      font-size: 17px;
    }

    .standard-panel {
      display: grid;
      grid-template-columns: 1.35fr 1fr 1fr;
      gap: 14px;
      margin-top: 14px;
    }

    .standard-card {
      min-height: 148px;
      padding: 22px;
      background: rgba(17, 22, 34, 0.72);
      border: 1px solid var(--line);
      border-radius: var(--radius-lg);
    }

    .standard-card.primary {
      background-image: linear-gradient(135deg, rgba(95, 111, 238, 0.25), rgba(139, 91, 222, 0.15));
      border-color: rgba(139, 151, 255, 0.22);
    }

    .standard-label {
      margin: 0;
      color: #aeb8ce;
      font-size: 12px;
      font-weight: 700;
      letter-spacing: 0.12em;
      text-transform: uppercase;
    }

    .standard-title {
      margin: 24px 0 0;
      font-size: 20px;
      line-height: 1.35;
      letter-spacing: -0.025em;
    }

    .standard-copy {
      margin: 9px 0 0;
      color: var(--subtle);
      font-size: 13px;
    }

    footer {
      width: 92%;
      max-width: 1100px;
      margin: 0 auto;
      padding: 0 0 32px;
      color: #596377;
      font-size: 12px;
      text-align: center;
    }

    @media (max-width: 900px) {
      main { padding-top: 38px; }
      .site-grid { grid-template-columns: repeat(3, 1fr); }
      .standard-panel { grid-template-columns: 1fr 1fr; }
      .standard-card.primary { grid-column: 1 / span 2; }
    }

    @media (max-width: 660px) {
      .engine-state { display: none; }
      main { width: 90%; padding-top: 28px; }
      h1 { font-size: clamp(38px, 12vw, 56px); }
      .hero-copy { font-size: 15px; }
      .address-guide { width: 100%; min-height: 58px; margin-top: 28px; }
      .key { display: none; }
      .quick-access { margin-top: 56px; }
      .section-note { display: none; }
      .site-grid { grid-template-columns: repeat(2, 1fr); }
      .standard-panel { grid-template-columns: 1fr; }
      .standard-card.primary { grid-column: auto; }
    }

    @media (max-width: 430px) {
      .site-grid { grid-template-columns: 1fr; }
      .site-card { min-height: 78px; }
    }
  </style>
</head>
<body>
  <div class="ambient" aria-hidden="true"></div>

  <header class="topbar">
    <a class="wordmark" href="about:home" aria-label="rENDER 主页">
      <span class="wordmark-mark">R</span>
      <span>rENDER</span>
    </a>
    <p class="engine-state"><span class="engine-state-dot"></span>Standards-first rendering engine</p>
  </header>

  <main>
    <section class="hero" aria-labelledby="home-title">
      <p class="eyebrow">New tab</p>
      <h1 id="home-title">打开网页，<span>看见标准本身。</span></h1>
      <p class="hero-copy">输入网址、域名或搜索内容。rENDER 会按照现代 HTML 与 CSS 标准解析和呈现页面。</p>
      <a class="address-guide" href="https://www.baidu.com/" aria-label="打开百度">
        <span class="search-symbol">⌕</span>
        <span class="address-copy">在上方地址栏输入网址或搜索内容</span>
        <kbd class="key">Ctrl</kbd><kbd class="key">L</kbd>
      </a>
    </section>

    <section class="quick-access" aria-labelledby="quick-title">
      <div class="section-heading">
        <h2 id="quick-title">快速访问</h2>
        <p class="section-note">常用中文互联网服务</p>
      </div>

      <nav class="site-grid" aria-label="常用网站">
        <a class="site-card" href="https://www.baidu.com/"><span class="site-icon tone-blue">百</span><span class="site-meta"><span class="site-name">百度</span><span class="site-host">baidu.com</span></span><span class="site-arrow">↗</span></a>
        <a class="site-card" href="https://www.qq.com/"><span class="site-icon tone-cyan">Q</span><span class="site-meta"><span class="site-name">腾讯网</span><span class="site-host">qq.com</span></span><span class="site-arrow">↗</span></a>
        <a class="site-card" href="https://www.163.com/"><span class="site-icon tone-red">易</span><span class="site-meta"><span class="site-name">网易</span><span class="site-host">163.com</span></span><span class="site-arrow">↗</span></a>
        <a class="site-card" href="https://www.sina.com.cn/"><span class="site-icon tone-orange">新</span><span class="site-meta"><span class="site-name">新浪</span><span class="site-host">sina.com.cn</span></span><span class="site-arrow">↗</span></a>
        <a class="site-card" href="https://www.bilibili.com/"><span class="site-icon tone-pink">B</span><span class="site-meta"><span class="site-name">哔哩哔哩</span><span class="site-host">bilibili.com</span></span><span class="site-arrow">↗</span></a>
        <a class="site-card" href="https://www.zhihu.com/"><span class="site-icon tone-blue">知</span><span class="site-meta"><span class="site-name">知乎</span><span class="site-host">zhihu.com</span></span><span class="site-arrow">↗</span></a>
        <a class="site-card" href="https://www.douban.com/"><span class="site-icon tone-green">豆</span><span class="site-meta"><span class="site-name">豆瓣</span><span class="site-host">douban.com</span></span><span class="site-arrow">↗</span></a>
        <a class="site-card" href="https://www.sohu.com/"><span class="site-icon tone-purple">狐</span><span class="site-meta"><span class="site-name">搜狐</span><span class="site-host">sohu.com</span></span><span class="site-arrow">↗</span></a>
        <a class="site-card" href="https://www.hao123.com/"><span class="site-icon tone-green">H</span><span class="site-meta"><span class="site-name">hao123</span><span class="site-host">hao123.com</span></span><span class="site-arrow">↗</span></a>
        <a class="site-card" href="https://www.tom.com/"><span class="site-icon tone-slate">T</span><span class="site-meta"><span class="site-name">TOM</span><span class="site-host">tom.com</span></span><span class="site-arrow">↗</span></a>
        <a class="site-card" href="https://www.people.com.cn/"><span class="site-icon tone-red">人</span><span class="site-meta"><span class="site-name">人民网</span><span class="site-host">people.com.cn</span></span><span class="site-arrow">↗</span></a>
        <a class="site-card" href="https://www.xinhuanet.com/"><span class="site-icon tone-cyan">新</span><span class="site-meta"><span class="site-name">新华网</span><span class="site-host">xinhuanet.com</span></span><span class="site-arrow">↗</span></a>
      </nav>

      <div class="standard-panel" aria-label="浏览器能力">
        <article class="standard-card primary">
          <p class="standard-label">Standards mode</p>
          <h3 class="standard-title">主页本身就是一份现代 Web 平台兼容性用例。</h3>
          <p class="standard-copy">语义化 HTML、响应式 Grid、Flexbox、自定义属性、渐变、滤镜与动态视口单位。</p>
        </article>
        <article class="standard-card">
          <p class="standard-label">HTML5</p>
          <h3 class="standard-title">语义优先</h3>
          <p class="standard-copy">不依赖站点特判，不绕过标准导航流程。</p>
        </article>
        <article class="standard-card">
          <p class="standard-label">CSS</p>
          <h3 class="standard-title">响应式呈现</h3>
          <p class="standard-copy">从桌面窗口到窄屏视口都保持清晰秩序。</p>
        </article>
      </div>
    </section>
  </main>

  <footer>rENDER · Built around the open web</footer>
</body>
</html>'''
