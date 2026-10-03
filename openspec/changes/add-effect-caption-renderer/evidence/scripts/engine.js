/* caption-sandbox 渲染器引擎脚本（模拟 HyperFrames 随包注入的引擎脚本）
 * 职责：拥有时间轴，按外部给定的毫秒时间点确定性 seek 页面中的所有 CSS 动画。
 * 该文件不属于组合（composition），由渲染器在加载组合后注入；组合自身不含任何脚本。
 */
(function () {
  function allAnimations() {
    return document.getAnimations ? document.getAnimations() : [];
  }
  window.__ENGINE__ = {
    version: 'sandbox-engine/1.0.0',
    async ready() {
      if (document.fonts && document.fonts.ready) {
        await document.fonts.ready;
      }
      // 让布局与首帧样式稳定
      await new Promise((r) => requestAnimationFrame(() => r()));
      return true;
    },
    seek(tMs) {
      const list = allAnimations();
      for (const a of list) {
        try {
          a.pause();
          a.currentTime = tMs;
        } catch (e) {
          /* 忽略不可 seek 的动画 */
        }
      }
      document.documentElement.setAttribute('data-seek-ms', String(tMs));
      return list.length;
    },
  };
})();
