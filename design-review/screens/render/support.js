// Minimal local renderer for Claude Design .dc.html boards (holes, sc-if, sc-for, dc-import, DCLogic).
(function () {
  class DCLogic {
    constructor(props) { this.props = props || {}; this.state = {}; }
    setState(u, cb) {
      const n = typeof u === 'function' ? u(this.state, this.props) : u;
      this.state = Object.assign({}, this.state, n);
      if (this.__rerender) this.__rerender();
      if (cb) cb();
    }
    renderVals() { return {}; }
  }
  window.DCLogic = DCLogic;
  const docs = {}, styled = new Set();
  let pending = 0;
  async function load(name) {
    if (!docs[name]) {
      const t = (await (await fetch(name + '.dc.html')).text()).replace(/\/_blob\/([0-9a-f]{32})/g, '/_blob/$1.png');
      docs[name] = new DOMParser().parseFromString(t, 'text/html');
    }
    return docs[name];
  }
  function lit(e, scope) {
    e = e.trim();
    if (e === 'true') return true;
    if (e === 'false') return false;
    if (e === 'null') return null;
    if (/^-?\d+(\.\d+)?$/.test(e)) return Number(e);
    if (/^(['"]).*\1$/.test(e)) return e.slice(1, -1);
    let v = scope;
    for (const k of e.split('.')) { if (v == null) return undefined; v = v[k]; }
    return v;
  }
  const HOLE = /\{\{([^}]*)\}\}/g;
  const whole = s => { const m = s.match(/^\s*\{\{([^}]*)\}\}\s*$/); return m ? m[1] : null; };
  const str = v => (v == null || v === false ? '' : typeof v === 'function' ? '' : String(v));
  const interp = (s, scope) => s.replace(HOLE, (_, e) => str(lit(e, scope)));
  const camel = s => s.replace(/-([a-z])/g, (_, c) => c.toUpperCase());
  function attrVal(v) {
    if (v === 'true') return true;
    if (v === 'false') return false;
    if (/^-?\d+(\.\d+)?$/.test(v)) return Number(v);
    return v;
  }
  function proc(node, scope, imports) {
    if (node.nodeType === 3) { if (node.nodeValue.includes('{{')) node.nodeValue = interp(node.nodeValue, scope); return [node]; }
    if (node.nodeType !== 1) return [node];
    const tag = node.tagName.toLowerCase();
    const kids = () => { const out = []; for (const c of [...node.childNodes]) out.push(...proc(c, scope, imports)); return out; };
    if (tag === 'sc-if') {
      const e = whole(node.getAttribute('value') || '');
      const v = e != null ? lit(e, scope) : node.getAttribute('value');
      return v ? kids() : [];
    }
    if (tag === 'sc-for') {
      const e = whole(node.getAttribute('list') || '');
      const list = (e != null ? lit(e, scope) : null) || [];
      const as = node.getAttribute('as') || 'item';
      const out = [];
      [...list].forEach((item, i) => {
        const s2 = Object.assign(Object.create(null), scope, { [as]: item, index: i });
        for (const c of [...node.childNodes]) out.push(...proc(c.cloneNode(true), s2, imports));
      });
      return out;
    }
    for (const a of [...node.attributes]) {
      if (!a.value.includes('{{')) continue;
      if (/^on/i.test(a.name)) { node.removeAttribute(a.name); continue; }
      const e = whole(a.value);
      if (e != null) {
        const v = lit(e, scope);
        if (a.name.startsWith('aria-') || a.name.startsWith('data-')) node.setAttribute(a.name, v == null ? '' : String(v));
        else if (v === false || v == null || typeof v === 'function') node.removeAttribute(a.name);
        else node.setAttribute(a.name, v === true ? '' : String(v));
      } else node.setAttribute(a.name, interp(a.value, scope));
    }
    if (tag === 'dc-import') {
      const props = {};
      for (const a of [...node.attributes]) if (a.name !== 'name' && !a.name.startsWith('hint-')) props[camel(a.name)] = attrVal(a.value);
      const host = document.createElement('div');
      host.style.display = 'contents';
      imports.push(() => mount(node.getAttribute('name'), props, host));
      return [host];
    }
    const out = kids();
    while (node.firstChild) node.removeChild(node.firstChild);
    for (const c of out) node.appendChild(c);
    return [node];
  }
  async function mount(name, overrides, host) {
    pending++;
    const doc = await load(name);
    const xdc = doc.querySelector('x-dc');
    const script = doc.querySelector('script[data-dc-script]');
    const meta = JSON.parse(script.getAttribute('data-props') || '{}');
    const props = {};
    for (const [k, v] of Object.entries(meta)) if (!k.startsWith('$') && v && 'default' in v) props[k] = v.default;
    Object.assign(props, overrides);
    if (!styled.has(name)) {
      styled.add(name);
      const h = xdc.querySelector('helmet');
      if (h) for (const c of [...h.children]) document.head.appendChild(document.importNode(c, true));
    }
    const Comp = new Function('DCLogic', script.textContent + '\n;return Component;')(DCLogic);
    const inst = new Comp(props);
    if (!inst.props) inst.props = props;
    if (!inst.state) inst.state = {};
    const tpl = [...xdc.childNodes].filter(n => !(n.nodeType === 1 && /^(helmet|script)$/i.test(n.tagName)));
    let queued = false;
    const render = async () => {
      const vals = inst.renderVals() || {};
      const scope = Object.assign(Object.create(null), vals);
      const imports = [];
      const nodes = [];
      for (const n of tpl) nodes.push(...proc(document.importNode(n, true), scope, imports));
      host.replaceChildren(...nodes);
      for (const f of imports) await f();
    };
    inst.__rerender = () => { if (queued) return; queued = true; setTimeout(() => { queued = false; render(); }, 0); };
    await render();
    if (inst.componentDidMount) { try { inst.componentDidMount(); } catch (e) { console.error(e); } }
    pending--;
  }
  window.__mountBoard = async (name, props) => {
    try { await mount(name, props || {}, document.body); window.__done = 'ok'; }
    catch (e) { window.__done = 'error: ' + (e && e.stack || e); }
  };
})();
