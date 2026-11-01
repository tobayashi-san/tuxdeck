// Added to every HTML page from the plugin file server (web::plugin_router). Property
// inspectors run in an iframe on that origin, apart from the configuration UI: the UI
// can't reach into the frame, so this connects the inspector when the UI asks and
// reports the height of its content.
(() => {
  const UI = __UI_ORIGINS__;
  if (window.parent === window) return;
  let ui = null;
  let raf = 0;
  let last = 0;

  /** Lowest content edge below `el` (viewport coords + offset), looking inside scroll containers. */
  function contentBottom(el, offset, depth) {
    let bottom = 0;
    for (const c of el.children) {
      if (/^(SCRIPT|STYLE|LINK|TEMPLATE|META|NOSCRIPT)$/.test(c.tagName)) continue;
      const cs = getComputedStyle(c);
      if (cs.display === 'none' || cs.position === 'fixed' || cs.position === 'absolute') continue;
      let b;
      if (/(auto|scroll)/.test(cs.overflowY) && depth < 4 && c.children.length) {
        b = contentBottom(c, offset + c.scrollTop, depth + 1) + parseFloat(cs.paddingBottom || 0) + parseFloat(cs.borderBottomWidth || 0);
        b = Math.max(b, c.getBoundingClientRect().top + offset);
      } else {
        b = c.getBoundingClientRect().bottom + offset;
      }
      bottom = Math.max(bottom, b + parseFloat(cs.marginBottom || 0));
    }
    return bottom;
  }

  // Elgato's sdpi.css pins html/body/.sdpi-wrapper to 100 % height and scrolls inside, so
  // body size == frame size. Measure the real content extent instead, plus the body's
  // bottom padding/margin; if a top-level scroll container still overflows at the
  // current size, grow by that amount.
  function fit() {
    const body = document.body;
    if (!body || !ui) return;
    const cs = getComputedStyle(body);
    const bottom = contentBottom(body, document.scrollingElement?.scrollTop || 0, 0);
    let target = Math.max(120, Math.ceil(Math.max(
      bottom + parseFloat(cs.paddingBottom || 0) + parseFloat(cs.marginBottom || 0),
      cs.overflowY === 'visible' ? body.scrollHeight : 0)));
    if (last && Math.abs(last - target) < 2) {
      let over = 0;
      for (const el of [body, ...body.children]) {
        if (/(auto|scroll)/.test(getComputedStyle(el).overflowY)) over = Math.max(over, el.scrollHeight - el.clientHeight);
      }
      target += over;
    }
    if (target !== last) {
      last = target;
      parent.postMessage({ tuxdeck: 'height', px: target }, ui);
    }
  }
  const schedule = () => {
    cancelAnimationFrame(raf);
    raf = requestAnimationFrame(() => { fit(); setTimeout(fit, 120); });
  };

  function watch() {
    const body = document.body;
    if (!body) return;
    const ro = new ResizeObserver(schedule);
    ro.observe(body);
    for (const c of body.children) ro.observe(c);
    new MutationObserver(schedule).observe(body, { subtree: true, childList: true, attributes: true, characterData: true });
    // Tab switches, accordions, late images/fonts
    for (const ev of ['click', 'input', 'change', 'transitionend']) document.addEventListener(ev, schedule, true);
    document.addEventListener('load', schedule, true);
    fit();
    setTimeout(fit, 300);
  }

  addEventListener('message', (e) => {
    if (e.source !== parent || !UI.includes(e.origin) || e.data?.tuxdeck !== 'connect' || ui) return;
    ui = e.origin;
    const reply = (m) => parent.postMessage(m, ui);
    if (typeof window.connectElgatoStreamDeckSocket !== 'function') {
      reply({ tuxdeck: 'missing' });
    } else {
      try {
        window.connectElgatoStreamDeckSocket(...e.data.args);
        reply({ tuxdeck: 'connected' });
      } catch (err) {
        reply({ tuxdeck: 'error', message: String(err?.message || err) });
      }
    }
    watch();
  });
})();
