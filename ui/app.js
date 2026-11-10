/* Yakusu interface. All state lives in the Rust core; this file asks for it
   with call(command, args) and draws it. Inside the desktop app the call goes
   through Tauri; in a browser (dev server) it goes over HTTP. */
(function () {
  'use strict';
  const TAURI = window.__TAURI__;
  const $ = (s, r = document) => r.querySelector(s);
  const $$ = (s, r = document) => Array.from(r.querySelectorAll(s));
  let S = null;              // last state
  let view = 'home';
  let previewLines = [];
  let previewIdx = 0;
  let previewBox = null;
  const linesState = { filter: 'all', q: '', page: 0 };
  let estimates = null;
  let keptShownFor = null;
  let translatorOpen = false;   // the translator card is expanded by hand
  let keyStatus = null;         // { cls, text } under the API key field
  let titlePickOpen = false;    // choosing which Kirikiri game this is
  const modelLists = {};        // provider -> model IDs it offers

  // Cards are rebuilt only when what they show changes, so a click is never
  // lost to a redraw and typing is never interrupted.
  const drawn = {};
  function changed(key, data) {
    const sig = JSON.stringify(data);
    if (drawn[key] === sig) return false;
    drawn[key] = sig;
    return true;
  }

  // ------------------------------------------------------------ bridge
  async function call(cmd, args = {}) {
    if (TAURI) {
      try { return await TAURI.core.invoke('call', { cmd, args }); }
      catch (e) { throw new Error(typeof e === 'string' ? e : (e && e.message) || String(e)); }
    }
    const r = await fetch('/api/call', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', 'X-Yakusu-Token': window.__YAKUSU_TOKEN__ || '' },
      body: JSON.stringify({ cmd, args }),
    });
    let data = {};
    try { data = await r.json(); } catch (e) { /* empty */ }
    if (!r.ok) throw new Error(data.error || `Request failed (${r.status})`);
    return data;
  }

  async function pickFolder(title) {
    if (TAURI && TAURI.dialog) {
      const p = await TAURI.dialog.open({ directory: true, multiple: false, title });
      return Array.isArray(p) ? p[0] : p;
    }
    if (view !== 'home') show('home');
    $('#gamePath').focus();
    toast('Paste the folder path into the box and press Open.');
    return null;
  }

  function h(tag, attrs, ...kids) {
    const el = document.createElement(tag);
    if (attrs) for (const [k, v] of Object.entries(attrs)) {
      if (v == null || v === false) continue;
      if (k === 'class') el.className = v;
      else if (k === 'text') el.textContent = v;
      else if (k.startsWith('on')) el.addEventListener(k.slice(2), v);
      else el.setAttribute(k, v === true ? '' : v);
    }
    for (const c of kids.flat(Infinity)) if (c != null && c !== false) el.append(c.nodeType ? c : document.createTextNode(String(c)));
    return el;
  }
  function svgUse(id, cls) {
    const ns = 'http://www.w3.org/2000/svg';
    const s = document.createElementNS(ns, 'svg');
    s.setAttribute('class', cls || 'branch');
    s.setAttribute('aria-hidden', 'true');
    const u = document.createElementNS(ns, 'use');
    u.setAttribute('href', '#' + id);
    s.append(u);
    return s;
  }
  function petals(container, n, once) {
    if (matchMedia('(prefers-reduced-motion: reduce)').matches) return;
    container.replaceChildren();
    for (let i = 0; i < n; i++) {
      const p = h('i', { class: 'petal' });
      const dur = 9 + ((i * 3.7) % 7);
      p.style.left = `${8 + ((i * 37) % 88)}%`;
      p.style.animationDuration = `${dur}s`;
      p.style.animationDelay = once ? `${(i * 0.35).toFixed(2)}s` : `-${((i * 2.9) % dur).toFixed(2)}s`;
      p.style.transform = `scale(${0.7 + ((i * 13) % 6) / 10})`;
      container.append(p);
    }
  }
  $$('.petals[data-petals]').forEach((el) => petals(el, +el.dataset.petals));

  // Light (sakura by day) or dark (yozakura). Saved per computer.
  function drawThemeSwitch() {
    const cur = window.__yakusuTheme.current();
    $$('[data-theme-pick]').forEach((b) => {
      const on = (b.dataset.themePick === 'dark') === (cur === 'dark');
      b.setAttribute('aria-pressed', String(on));
      b.setAttribute('aria-checked', String(on));
    });
  }
  $$('[data-theme-pick]').forEach((b) => b.addEventListener('click', () => { window.__yakusuTheme.set(b.dataset.themePick); drawThemeSwitch(); }));
  drawThemeSwitch();

  let toastTimer;
  function toast(msg, err) {
    const t = $('#toast');
    t.textContent = msg;
    t.className = 'toast show' + (err ? ' err' : '');
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => (t.className = 'toast'), err ? 7000 : 3500);
  }
  async function safe(fn) {
    try { return await fn(); } catch (e) { toast(e.message, true); }
  }

  const money = (x) => x == null ? '—' : (x < 0.01 && x > 0 ? 'under $0.01' : '$' + x.toFixed(x < 10 ? 2 : 0));
  const range = (r) => !r ? null : (r[1] < 0.01 ? 'under $0.01' : r[0] < 0.01 ? `up to ${money(r[1])}` : `about ${money(r[0])} to ${money(r[1])}`);
  const num = (n) => (n || 0).toLocaleString('en-US');
  const cap = (t) => t ? t[0].toUpperCase() + t.slice(1) : t;
  const plural = (n, one, many) => `${num(n)} ${n === 1 ? one : (many || one + 's')}`;

  // ------------------------------------------------------------ navigation
  function show(v) {
    view = v;
    $$('.view').forEach((s) => s.classList.toggle('active', s.dataset.view === v));
    $$('.rail [data-view]').forEach((b) => { if (b.dataset.view !== v) b.removeAttribute('aria-current'); else b.setAttribute('aria-current', 'page'); });
    $('#main').scrollTop = 0;
    if (S) render();
    if (v === 'home') loadEstimates();
    if (v === 'style') loadStyle();
    if (v === 'test') loadPreview();
    if (v === 'lines') loadLines();
    if (v === 'activity') renderLog();
  }
  $$('.rail [data-view]').forEach((b) => b.addEventListener('click', () => show(b.dataset.view)));

  // ------------------------------------------------------------ polling
  let lastFinished;
  async function refresh() {
    try { S = await call('state'); } catch (e) { return; }
    render();
    const j = S.job || {};
    const fin = j.finished;
    if (lastFinished !== undefined && !j.running && fin && fin !== lastFinished) onJobDone();
    if (!j.running) lastFinished = fin || null;
    else if (lastFinished === undefined) lastFinished = null;
  }
  function loop() { refresh().finally(() => setTimeout(loop, (S && S.job && S.job.running) ? 600 : 2500)); }

  const DONE_MSG = { extract: 'Scripts read.', style: 'Style sheet drafted.', refit: 'Applied to the game.',
    restore: 'Original game restored.', build: 'Patch rebuilt and installed.', glossary: 'Name spellings fixed.',
    retranslate: 'Lines retranslated.', translate: 'Translation saved.', review: 'Review finished.', fit: 'Long lines shortened.' };
  function onJobDone() {
    const j = S.job;
    if (j.need) needSetup(j.need, j.error);
    else if (j.error) toast(j.error, true);
    else if (j.stopped) toast('Stopped. Everything translated so far is saved.');
    else if (j.action === 'auto') toast(j.result && j.result.installed ? 'Translation installed. Launch the game!' : 'Translation finished.');
    else if (j.action === 'test') toast(j.result && j.result.installed ? 'Test patch installed. Launch the game to check it.' : 'Test lines translated.');
    else toast(DONE_MSG[j.action] || 'Done.');
    if (!j.error && !j.stopped && j.result && j.result.complete) showKept(j.result);
    estimates = null;
    loadEstimates();
    if (view === 'style') loadStyle();
    if (view === 'test') loadPreview();
    if (view === 'lines') loadLines();
  }

  // Something has to be set up before the job can run: put that control in
  // front of the user instead of only reporting an error.
  function needSetup(need, msg) {
    if (need === 'key' || need === 'model') {
      translatorOpen = true;
      if (need === 'key') keyStatus = { cls: 'bad', text: msg };
      show('home');
      const el = $(need === 'key' ? '#apiKey' : '#tmodel') || $('#apiKey');
      if (el) { el.scrollIntoView({ block: 'center' }); el.focus(); }
    } else if (need === 'title') {
      titlePickOpen = true;
      show('home');
    }
    toast(msg, true);
  }

  // ------------------------------------------------------------ render
  function render() {
    const p = S.project;
    $('#railGame').textContent = p ? p.name : 'No game open';
    const foot = $('#railFoot');
    foot.replaceChildren();
    if (p && p.extracted) {
      foot.append(h('div', null, h('b', null, num(p.translated)), ` of ${num(p.total)} lines`));
      foot.append(h('div', null, h('b', null, money(p.spent)), ' spent'));
      if (p.installed) foot.append(h('div', { class: 'installed', text: 'Patch installed' }));
    }
    const np = $('#navProblems');
    np.hidden = !(p && p.problems);
    np.textContent = p && p.problems ? num(p.problems) : '';
    $('#versionLine').textContent = S.version ? `Yakusu ${S.version}` : '';
    renderJob();
    renderHome();
    renderSettings();
    renderTest();
    if (view === 'activity') renderLog();
  }

  const STAGE = { extract: 'Reading the scripts', style: 'Writing the style sheet', translate: 'Translating', review: 'Reviewing',
    fit: 'Fitting text to the box', build: 'Building the patch', test: 'Test patch', auto: 'Getting ready', refit: 'Building the patch',
    restore: 'Restoring', glossary: 'Checking name spellings', names: 'Naming the characters', retranslate: 'Retranslating' };
  function renderJob() {
    const j = S.job || {};
    $('#jobbar').hidden = !j.running;
    renderBanner();
    if (!j.running) { document.title = 'Yakusu'; return; }
    $('#jobStage').textContent = STAGE[j.stage] || STAGE[j.action] || 'Working';
    $('#jobMsg').textContent = j.message && j.message !== STAGE[j.stage] ? j.message : '';
    const fill = $('#jobFill');
    const numEl = $('#jobNum');
    if (j.total) {
      const pct = Math.round((100 * j.done) / j.total);
      fill.classList.remove('indeterminate');
      fill.style.width = pct + '%';
      const parts = [h('span', { text: `${num(j.done)} of ${num(j.total)}` })];
      if (j.spent != null) parts.push(h('span', { class: 'money', text: money(j.spent) }));
      const elapsed = Date.now() / 1000 - (j.stage_started || j.started);
      if (j.done > 0 && j.done < j.total && elapsed > 8) {
        const left = (elapsed / j.done) * (j.total - j.done);
        parts.push(h('span', { text: left < 90 ? `about ${Math.max(10, Math.round(left / 10) * 10)} s left` : `about ${duration(left)} left` }));
      }
      numEl.replaceChildren(...parts);
      document.title = `${pct}% · Yakusu`;
    } else {
      fill.classList.add('indeterminate');
      fill.style.width = '';
      numEl.replaceChildren();
      document.title = 'Working · Yakusu';
    }
  }
  function duration(sec) {
    const m = Math.round(sec / 60);
    if (m < 60) return `${m} min`;
    return `${Math.floor(m / 60)} h ${m % 60} min`;
  }
  $('#jobStop').addEventListener('click', () => safe(async () => { await call('cancel'); $('#jobMsg').textContent = 'Stopping after the requests in flight…'; }));

  // A failed job leaves a banner until dismissed, with a way to try again.
  let bannerDismissed = null;
  const RETRYABLE = ['extract', 'style', 'test', 'refit', 'auto', 'translate', 'review', 'fit', 'build', 'restore', 'glossary'];
  function renderBanner() {
    const j = S.job || {};
    // The game's own card explains a failed read; setup problems open their control.
    const showIt = !j.running && j.error && bannerDismissed !== j.finished && j.need !== 'title';
    $('#banner').hidden = !showIt;
    if (!showIt) return;
    $('#bannerTitle').textContent = j.need === 'key' ? 'An API key is needed' : j.need === 'model' ? 'A model is needed' :
      ({ extract: 'Couldn\'t read the scripts', style: 'Couldn\'t write the style sheet',
        test: 'The test patch didn\'t finish', auto: 'Translation stopped', translate: 'Translation stopped', review: 'Review stopped',
        build: 'Couldn\'t build the patch', refit: 'Couldn\'t apply the changes', restore: 'Couldn\'t restore the game' })[j.action] || 'Something went wrong';
    $('#bannerMsg').textContent = j.error + (j.action === 'auto' && !j.need ? ' Everything translated so far is saved: press Try again to pick up where it stopped.' : '');
    $('#bannerRetry').hidden = !RETRYABLE.includes(j.action) || !!j.need;
  }
  $('#bannerClose').addEventListener('click', () => { bannerDismissed = S.job.finished; renderBanner(); });
  $('#bannerLog').addEventListener('click', () => show('activity'));
  $('#bannerRetry').addEventListener('click', () => { bannerDismissed = S.job.finished; run(S.job.action); });

  // ------------------------------------------------------------ the game
  $('#browseGame').addEventListener('click', () => safe(async () => {
    const p = await pickFolder('Choose the game folder');
    if (p) await openGame(p);
  }));
  $('#pathForm').addEventListener('submit', (e) => {
    e.preventDefault();
    const v = $('#gamePath').value.trim().replace(/^"|"$/g, '');
    if (v) safe(() => openGame(v));
  });
  async function openGame(path) {
    await call('open', { game_dir: path });
    $('#gamePath').value = '';
    estimates = null;
    titlePickOpen = false;
    $('#kept').replaceChildren();
    await refresh();
    show('home');
  }
  // Back to the empty picker. The game keeps its translations and stays in the recent list.
  async function closeGame() {
    await call('close');
    estimates = null;
    titlePickOpen = false;
    translatorOpen = false;
    keyStatus = null;
    $('#kept').replaceChildren();
    await refresh();
    show('home');
    $('#gamePath').value = '';
  }
  const picker = $('#picker');
  if (TAURI && TAURI.webview) {
    // The desktop window hands over real paths for dropped folders.
    TAURI.webview.getCurrentWebview().onDragDropEvent((ev) => {
      const t = ev.payload.type;
      if (t === 'enter' || t === 'over') picker.classList.add('drag');
      else picker.classList.remove('drag');
      if (t === 'drop' && ev.payload.paths && ev.payload.paths[0]) safe(() => openGame(ev.payload.paths[0]));
    });
  } else {
    ['dragenter', 'dragover'].forEach((ev) => picker.addEventListener(ev, (e) => { e.preventDefault(); picker.classList.add('drag'); }));
    ['dragleave', 'drop'].forEach((ev) => picker.addEventListener(ev, () => picker.classList.remove('drag')));
    picker.addEventListener('drop', (e) => { e.preventDefault(); toast('The browser doesn\'t share folder paths. Paste the path instead.', true); });
  }
  // Pasting a path anywhere on the Translate page opens it.
  document.addEventListener('paste', (e) => {
    if (view !== 'home' || /^(INPUT|TEXTAREA)$/.test((document.activeElement || {}).tagName || '')) return;
    const t = (e.clipboardData.getData('text') || '').trim().replace(/^"|"$/g, '');
    if (/^([A-Za-z]:[\\/]|\/|\\\\)/.test(t)) { $('#gamePath').value = t; safe(() => openGame(t)); }
  });

  function usableGame() {
    const p = S.project;
    return !!(p && p.engine && p.engine.engine_id !== 'unsupported');
  }
  function renderHome() {
    const p = S.project;
    // A folder Yakusu can't read keeps the full picker right under the explanation.
    const usable = usableGame();
    $('#home').classList.toggle('has-game', usable);
    $('#home').classList.toggle('no-engine', !!p && !usable);
    $('#homeTitle').textContent = p ? p.name : 'Translate a visual novel';
    $('#homeLead').hidden = !!p;
    picker.classList.toggle('compact', usable);
    $('#browseGame').textContent = usable ? 'Open another game' : p ? 'Choose another folder' : 'Choose game folder';
    renderGame();
    renderTranslator();
    renderRun();
    renderRecent();
  }

  function renderRecent() {
    const items = S.recent || [];
    if (!changed('recent', [items, !!S.project])) return;
    const box = $('#recent');
    if (!items.length) { box.replaceChildren(); return; }
    box.replaceChildren(h('div', { class: 'recent' }, h('h2', { text: S.project ? 'Other games' : 'Recent games' }),
      h('ul', null, items.map((r) => h('li', { class: r.exists ? '' : 'missing' },
        h('div', { style: 'min-width:0' }, h('div', { class: 'r-name', text: r.name }), h('div', { class: 'r-path', text: r.game_dir, title: r.game_dir })),
        h('span', { class: 'r-meta', text: r.exists ? [r.engine, r.lines ? `${num(r.lines)} lines` : null].filter(Boolean).join(', ') : 'Folder not found' }),
        h('div', { class: 'inline' },
          r.exists ? h('button', { class: 'btn', onclick: () => safe(() => openGame(r.game_dir)) }, 'Open') : null,
          h('button', { class: 'icon-x', title: 'Remove from this list (translations are kept)', 'aria-label': `Remove ${r.name} from the list`,
            onclick: () => safe(async () => { await call('forget', { game_dir: r.game_dir }); refresh(); }) }, '×')))))));
  }

  function supportBadge(s) {
    const t = { auto: 'Automatic', experimental: 'Experimental', manual: 'Needs a manual step' }[s] || s;
    return h('span', { class: 'badge ' + s, text: t });
  }
  function isTyping(root) {
    const a = document.activeElement;
    return a && root.contains(a) && /^(INPUT|TEXTAREA|SELECT)$/.test(a.tagName);
  }
  function stat(v, l) { return h('div', { class: 'stat' }, h('b', { text: v }), h('span', { text: l })); }

  function renderGame() {
    const box = $('#gameCard');
    const p = S.project;
    if (!p) { if (changed('game', null)) box.replaceChildren(); return; }
    const j = S.job || {};
    const eng = p.engine;
    const det = (eng && eng.details) || {};
    const reading = j.running && j.action === 'extract';
    const readError = !j.running && j.action === 'extract' && j.error ? j.error : null;
    const needTitle = eng && eng.engine_id === 'kirikiri' && (titlePickOpen || (!j.running && j.need === 'title'));
    if (isTyping(box)) return;
    if (!changed('game', [p.game_dir, eng, p.stats, p.box, p.extracted, S.matches, reading, readError, needTitle, !!j.running])) return;
    const card = h('div', { class: 'card detect' });
    const usable = eng && eng.engine_id !== 'unsupported';
    card.append(h('div', { class: 'detect-head' },
      h('div', null, h('h2', { text: eng ? eng.name : 'Engine not recognised' }), h('div', { class: 'path', text: p.game_dir })),
      h('div', { class: 'detect-side' }, eng ? supportBadge(eng.support) : null,
        h('button', { class: usable ? 'btn btn-quiet' : 'btn btn-primary', id: 'closeGame', disabled: !!j.running,
          title: 'Close this game and pick another folder. Its translations are kept.', onclick: () => safe(closeGame) }, 'Choose a different game'))));
    if (eng && eng.note) card.append(h('p', { class: 'note', text: eng.note }));
    if (!eng) {
      const u = S.matches.find((m) => m.engine_id === 'unsupported');
      card.append(h('p', { class: 'note', text: u ? `${u.name}: ${u.note}` :
        'Yakusu didn\'t recognise this game\'s engine. If you have its script files extracted (for example with GARbro), translate them directly.' }));
    }
    if (needTitle) {
      card.append(titlePicker(!!det.xp3_title));
    } else if (eng && eng.engine_id === 'kirikiri' && det.xp3_title) {
      card.append(h('p', { class: 'note' }, `Encrypted archives, decrypted with the profile for “${det.xp3_title}”. `,
        h('button', { class: 'btn btn-quiet', onclick: () => { titlePickOpen = true; render(); } }, 'Change')));
    }
    if (reading) {
      card.append(h('p', { class: 'note reading', role: 'status', text: 'Reading the scripts… this takes a moment for big games.' }));
    } else if (readError && !needTitle) {
      card.append(h('div', { class: 'callout bad' }, h('b', { text: 'Couldn\'t read the scripts. ' }), readError));
    }
    if (p.stats && !reading) {
      const st = p.stats;
      card.append(h('div', { class: 'stats' },
        stat(num(st.lines), 'lines to translate'),
        stat(num(st.files), 'script files'),
        stat(num(st.jp_chars), 'Japanese characters'),
        stat(p.box ? `${p.box.width} × ${p.box.lines}` : '—', 'text box size')));
    }
    // Ways out when the automatic reading isn't right.
    const others = S.matches.filter((m) => eng && m.engine_id !== eng.engine_id && m.engine_id !== 'unsupported');
    const tools = h('div', { class: 'alt-list' });
    if (eng && eng.engine_id !== 'unsupported' && !reading) {
      tools.append(h('button', { class: p.extracted ? 'btn btn-quiet' : 'btn btn-primary', onclick: () => run('extract') }, p.extracted ? 'Read the scripts again' : 'Read the scripts'));
    }
    others.forEach((m) => tools.append(h('button', { class: 'btn btn-quiet', onclick: () => safe(async () => { await call('engine', { engine_id: m.engine_id }); refresh(); }) }, `Read as ${m.name}`)));
    tools.append(h('button', { class: 'btn btn-quiet', onclick: () => safe(async () => {
      const path = await pickFolder('Choose the folder with the extracted script files');
      if (path) { await call('engine', { scripts_folder: path }); refresh(); }
    }) }, 'Use extracted script files instead'));
    card.append(tools);
    box.replaceChildren(card);
  }

  function titlePicker(hasTitle) {
    const wrap = h('div', { class: 'title-pick' },
      h('b', { text: hasTitle ? 'Which game is this?' : 'This game\'s archives are encrypted. Which game is it?' }),
      h('p', { class: 'hint', text: 'Yakusu knows the encryption of about 500 Kirikiri games. Search by English or Japanese title, then pick the exact game and edition.' }));
    const input = h('input', { class: 'input', type: 'search', placeholder: 'Search titles', 'aria-label': 'Search game titles' });
    const list = h('ul');
    let t;
    input.addEventListener('input', () => { clearTimeout(t); t = setTimeout(search, 200); });
    async function search() {
      const r = await call('xp3_games', { q: input.value.trim() });
      list.replaceChildren(...r.games.map((g) => h('li', null, h('button', { onclick: () => safe(async () => {
        titlePickOpen = false;
        // Setting the title reads the scripts again with that profile.
        await call('engine', { xp3_title: g.name });
        toast(`Using the profile for ${g.name}.`);
        refresh();
      }) }, g.name, g.title ? h('span', { class: 'hint', text: '  ' + g.title }) : null))));
    }
    wrap.append(input, list);
    search();
    return wrap;
  }

  // ------------------------------------------------------------ the translator
  const modelOf = (s, which) => ((s[which] || {})[s.provider] || '').trim();
  function translatorReady() {
    const s = S.settings;
    if (s.provider === 'local') return true;            // the loaded model is found when the run starts
    if (!s.keys[s.provider]) return false;
    return s.provider === 'anthropic' || !!modelOf(s, 'translate_model');
  }
  function translatorSummary() {
    const s = S.settings;
    const prov = S.providers[s.provider];
    const bits = [prov.label];
    if (s.provider === 'anthropic') bits.push((S.presets[s.preset] || {}).label || s.preset);
    else bits.push(modelOf(s, 'translate_model') || (s.provider === 'local' ? 'whichever model is loaded' : 'no model chosen'));
    if (s.provider !== 'local') bits.push(s.keys[s.provider] ? `key ${s.keys[s.provider]}` : 'no key yet');
    return bits.join(' · ');
  }
  async function loadModels(force) {
    const prov = S.settings.provider;
    if (prov === 'anthropic' || (modelLists[prov] && !force)) return;
    if (prov !== 'local' && !S.settings.keys[prov]) return;
    modelLists[prov] = modelLists[prov] || [];
    try { modelLists[prov] = (await call('models')).models || []; } catch (e) { /* the field still takes a typed name */ }
    if (S) renderTranslator();
  }
  async function saveKey(value) {
    const prov = S.settings.provider;
    if (value) await call('settings', { provider: prov, api_key: value });
    keyStatus = { cls: '', text: 'Checking the key…' };
    await refresh();
    try {
      const r = await call('test_key');
      keyStatus = { cls: 'ok', text: `The key works (${r.model}).` };
      translatorOpen = false;
      toast('The key works.');
    } catch (e) {
      keyStatus = { cls: 'bad', text: e.message };
      translatorOpen = true;
    }
    estimates = null;
    await refresh();
    loadEstimates();
    loadModels(true);
  }

  function renderTranslator() {
    const box = $('#translatorCard');
    const p = S.project;
    if (!usableGame()) { if (changed('translator', null)) box.replaceChildren(); return; }
    const s = S.settings;
    const ready = translatorReady();
    const open = translatorOpen || !ready;
    const models = modelLists[s.provider] || [];
    const stats = !!p.stats;
    if (!changed('translator', [s.provider, s.keys, s.preset, s.review, s.translate_model, open, keyStatus, models, stats, estimates])) return;
    const prov = S.providers[s.provider];
    const typed = $('#apiKey') ? $('#apiKey').value : '';
    // Whatever is being typed survives the redraw.
    const active = document.activeElement;
    const focusId = active && box.contains(active) && active.id ? active.id : null;
    const typedModel = focusId === 'tmodel' ? active.value : null;
    const refocus = () => {
      const el = focusId && document.getElementById(focusId);
      if (!el) return;
      if (typedModel != null) el.value = typedModel;
      el.focus();
    };
    const card = h('div', { class: 'card translator' });
    card.append(h('div', { class: 'card-head' },
      h('div', null, h('h2', { text: 'Translator' }), open ? null : h('div', { class: 'card-sub', text: translatorSummary() })),
      ready ? h('button', { class: 'btn btn-quiet', onclick: () => { translatorOpen = !open; render(); } }, open ? 'Done' : 'Change') : null));
    if (!open) { box.replaceChildren(card); return; }

    card.append(h('div', { class: 'field-group' },
      h('div', { class: 'seg', role: 'radiogroup', 'aria-label': 'Provider' }, Object.entries(S.providers).map(([id, pr]) => h('button', {
        'aria-pressed': String(s.provider === id), role: 'radio', 'aria-checked': String(s.provider === id),
        onclick: () => safe(async () => { keyStatus = null; translatorOpen = true; await call('settings', { provider: id }); estimates = null; await refresh(); loadEstimates(); loadModels(); }),
      }, pr.label)))));

    if (s.provider !== 'local') {
      const saved = s.keys[s.provider];
      const key = h('input', { class: 'input', type: 'password', id: 'apiKey', autocomplete: 'off', spellcheck: 'false',
        placeholder: saved ? `Saved (${saved}). Paste a new key to replace it.` : `Paste your key (${prov.key_hint})` });
      key.value = typed;
      const go = () => safe(() => saveKey(key.value.trim()));
      key.addEventListener('keydown', (e) => { if (e.key === 'Enter') go(); });
      // A pasted key is saved and checked straight away.
      key.addEventListener('paste', () => setTimeout(() => { if (key.value.trim().length > 12) go(); }, 0));
      card.append(h('div', { class: 'field-group' }, h('label', { class: 'label', for: 'apiKey', text: 'API key' }),
        h('div', { class: 'key-row' }, key, h('button', { class: saved ? 'btn' : 'btn btn-primary', onclick: go }, saved ? 'Test' : 'Save')),
        keyStatus ? h('div', { class: 'key-status ' + keyStatus.cls, role: 'status', text: keyStatus.text }) : null,
        prov.key_url ? h('p', { class: 'hint' }, 'Get a key at ', h('a', { href: prov.key_url, target: '_blank', rel: 'noopener', text: prov.key_url.replace('https://', '') }),
          '. You pay the provider directly; the key is stored only on this computer.') : null));
    }

    if (s.provider === 'anthropic') {
      card.append(h('div', { class: 'field-group' }, h('div', { class: 'label', text: 'Quality' }),
        h('div', { class: 'presets', role: 'radiogroup', 'aria-label': 'Quality' }, Object.entries(S.presets).map(([id, pre]) => {
          const est = estimates && estimates[id];
          return h('button', { class: 'preset', role: 'radio', 'aria-checked': String(s.preset === id),
            onclick: () => safe(async () => { await call('settings', { preset: id, review: !!pre.review }); await refresh(); loadEstimates(); }) },
            h('b', { text: pre.label }),
            h('span', { class: 'blurb', text: pre.blurb }),
            h('span', { class: 'cost', text: est && est.total ? cap(range(est.total)) : (stats ? 'Estimating…' : '') }));
        }))));
    } else {
      const listId = 'modelList';
      const tm = h('input', { class: 'input', id: 'tmodel', list: listId, value: modelOf(s, 'translate_model'), autocomplete: 'off', spellcheck: 'false',
        placeholder: s.provider === 'local' ? 'Automatic: whichever model is loaded' : 'Model ID, for example ' + ({ openai: 'gpt-5-mini', openrouter: 'google/gemini-2.5-flash' }[s.provider] || 'from your provider') });
      tm.addEventListener('change', () => safe(async () => { await call('settings', { translate_model: tm.value }); estimates = null; await refresh(); loadEstimates(); }));
      card.append(h('div', { class: 'field-group' }, h('label', { class: 'label', for: 'tmodel', text: 'Model' }), tm,
        h('datalist', { id: listId }, models.map((m) => h('option', { value: m }))),
        h('p', { class: 'hint', text: s.provider === 'local'
          ? 'Start the local server in LM Studio (Developer tab) or Ollama first. Local models are free but slower, and quality depends on the model.'
          : 'A review model, prices and the API address are in Settings.' })));
    }
    box.replaceChildren(card);
    refocus();
  }

  // ------------------------------------------------------------ settings
  $('#budget').addEventListener('change', (e) => safe(() => call('settings', { budget_usd: parseFloat(e.target.value) || 0 })));
  const ADV = [['#batchSize', 'batch_size', '#bsVal', 40], ['#parallel', 'parallel', '#parVal', 4], ['#contextLines', 'context_lines', '#ctxVal', 10]];
  for (const [id, key, lab] of ADV) {
    $(id).addEventListener('input', () => { $(lab).textContent = $(id).value; });
    $(id).addEventListener('change', () => safe(async () => { await call('settings', { [key]: +$(id).value }); estimates = null; loadEstimates(); }));
  }
  function renderSettings() {
    if (view !== 'settings') return;
    const s = S.settings;
    const b = $('#budget');
    if (document.activeElement !== b) b.value = s.budget_usd || '';
    for (const [id, key, lab, def] of ADV) {
      if (document.activeElement !== $(id)) $(id).value = s[key] != null ? s[key] : def;
      $(lab).textContent = $(id).value;
    }
    const f = $('#providerExtras');
    if (isTyping(f)) return;
    if (!changed('extras', [s.provider, s.translate_model, s.review_model, s.base_urls, s.prices])) return;
    f.replaceChildren();
    if (s.provider === 'anthropic') return;
    const prov = S.providers[s.provider];
    const rm = h('input', { class: 'input', id: 'rmodel', value: modelOf(s, 'review_model'), placeholder: 'Optional: a stronger model that edits every line' });
    const bu = h('input', { class: 'input', id: 'burl', value: (s.base_urls || {})[s.provider] || prov.base_url });
    rm.addEventListener('change', () => safe(async () => { await call('settings', { review_model: rm.value, review: !!rm.value.trim() }); toast('Saved.'); refresh(); }));
    bu.addEventListener('change', () => safe(async () => { await call('settings', { base_url: bu.value }); toast('Saved.'); refresh(); }));
    // Paid providers: the price per million tokens, for estimates and the spending limit.
    const priceRow = (model) => {
      const cur = (s.prices || {})[model] || [];
      const pin = h('input', { class: 'input', inputmode: 'decimal', value: cur[0] ?? '', placeholder: 'Input', 'aria-label': `${model} input price, USD per million tokens` });
      const pout = h('input', { class: 'input', inputmode: 'decimal', value: cur[1] ?? '', placeholder: 'Output', 'aria-label': `${model} output price, USD per million tokens` });
      const save = () => safe(async () => {
        const both = pin.value.trim() !== '' && pout.value.trim() !== '';
        await call('settings', { prices: { [model]: both ? [pin.value, pout.value] : null } });
        toast('Saved.'); estimates = null; refresh();
      });
      [pin, pout].forEach((x) => x.addEventListener('change', save));
      return h('div', { class: 'field-group' }, h('div', { class: 'label', text: `Price of ${model}` }),
        h('div', { class: 'two' }, pin, pout), h('p', { class: 'hint', text: 'In $ per million tokens: input, then output.' }));
    };
    const tmName = modelOf(s, 'translate_model');
    const rmName = modelOf(s, 'review_model');
    f.append(h('div', { class: 'field-group' }, h('div', { class: 'label', text: prov.label })),
      h('div', { class: 'field-group' }, h('label', { class: 'label', for: 'rmodel', text: 'Review model' }), rm),
      h('div', { class: 'field-group' }, h('label', { class: 'label', for: 'burl', text: 'API address' }), bu));
    if (s.provider !== 'local') {
      if (tmName) f.append(priceRow(tmName));
      if (rmName && rmName !== tmName) f.append(priceRow(rmName));
      f.append(h('p', { class: 'hint', text: 'Enter each model\'s price (from your provider\'s pricing page) to see cost estimates and use a spending limit. Without a price, a run with a spending limit won\'t start.' }));
    }
  }

  // ------------------------------------------------------------ translate
  async function loadEstimates() {
    if (!S || !S.project || !S.project.stats) return;
    try { estimates = await call('estimate', { all: true }); } catch (e) { estimates = null; }
    if (S) { renderTranslator(); renderRun(); }
    loadNotes();
  }
  async function loadNotes() {
    let r;
    try { r = await call('preflight'); } catch (e) { return; }
    if (!changed('notes', [r, S.project && [S.project.translated, S.project.reviewed], reviewOn()])) return;
    const items = (r.notes || []).map((n) => h('li', { class: n.level }, n.text));
    // While the main button still has work to do it takes care of these
    // itself; they're only worth showing for what's left afterwards.
    const p = S.project || {};
    const pending = !p.total || p.translated < p.total || (reviewOn() && p.reviewed < p.total);
    if (pending) { r.glossary_issues = 0; r.overflow = 0; }
    if (r.glossary_issues) {
      items.push(h('li', { class: 'warn' }, `${plural(r.glossary_issues, 'line')} ${r.glossary_issues === 1 ? 'doesn\'t' : 'don\'t'} use the agreed spelling of a name or term.`,
        h('button', { class: 'btn btn-quiet', onclick: () => { linesState.filter = 'problems'; show('lines'); } }, 'See them'),
        h('button', { class: 'btn', onclick: () => run('glossary') }, 'Fix them')));
    }
    if (r.overflow && S.project && S.project.translated) {
      items.push(h('li', { class: 'warn' }, `${plural(r.overflow, 'line')} won't fit the text box.`,
        h('button', { class: 'btn btn-quiet', onclick: () => show('test') }, 'Text box'),
        h('button', { class: 'btn', onclick: () => run('fit') }, 'Shorten them')));
    }
    $('#runNotes').replaceChildren(...items);
  }

  // A second model edits every line (the Recommended and Best presets, or a review model).
  function reviewOn() {
    const s = S.settings;
    if (s.review === false) return false;
    return s.provider === 'anthropic' ? !!(S.presets[s.preset] || {}).review : !!modelOf(s, 'review_model');
  }

  function renderRun() {
    const box = $('#runCard');
    const p = S.project;
    const j = S.job || {};
    if (!p || !p.stats) {
      if (changed('run', null)) { box.replaceChildren(); $('#runNotes').replaceChildren(); $('#manualNote').replaceChildren(); drawn.notes = null; }
      return;
    }
    const s = S.settings;
    const est = estimates && (s.provider === 'anthropic' ? estimates[s.preset] : estimates.current);
    const ready = translatorReady();
    const busy = !!j.running;
    const allDone = p.total > 0 && p.translated >= p.total;
    const reviewing = reviewOn();
    if (!changed('run', [p.total, p.translated, p.reviewed, p.problems, p.spent, p.installed, p.manual_output, est, ready, busy, reviewing, j.action])) return;
    const pct = (a, b) => (b ? Math.round((100 * a) / b) : 0);
    const row = (label, a, b, cls) => h('div', { class: 'progress-row' }, h('span', { text: label }),
      h('div', { class: 'bar ' + (cls || '') }, h('i', { style: `width:${pct(a, b)}%` })), h('span', { class: 'num', text: `${num(a)} of ${num(b)}` }));
    const card = h('div', { class: 'card run-summary' });
    if (p.translated > 0) {
      card.append(row('Translated', p.translated, p.total));
      if (reviewing) card.append(row('Reviewed', p.reviewed, p.total, 'ok'));
    }
    const remaining = p.total - p.translated;
    const costLeft = est && est.total && est.total[1] > 0 ? range(est.total) : null;
    card.append(h('p', { class: 'estimate' },
      allDone ? 'Every line is translated. ' : p.translated ? [h('b', { text: num(remaining) }), remaining === 1 ? ' line left. ' : ' lines left. '] : null,
      costLeft ? [allDone ? 'Finishing the review should cost ' : p.translated ? 'The rest should cost ' : 'Translating the whole game should cost ', h('b', { text: costLeft }), '. '] :
        (!allDone && s.provider === 'local' ? 'A local model costs nothing. ' : null),
      p.spent > 0 ? ['Spent so far: ', h('b', { text: money(p.spent) }), '. '] : null,
      p.problems ? [h('a', { href: '#', onclick: (e) => { e.preventDefault(); linesState.filter = 'problems'; show('lines'); } }, `${plural(p.problems, 'line')} need a look`), '.'] : null));
    const main = h('button', { class: 'btn btn-primary btn-lg', id: 'runAuto', disabled: busy, onclick: () => run('auto') },
      busy && (j.action === 'auto' || j.action === 'test') ? 'Working…' : allDone ? (costLeft ? 'Finish and install' : 'Rebuild and install') : p.translated ? 'Continue translating' : 'Translate the game');
    const actions = h('div', { class: 'row-actions' }, main);
    if (!p.translated) actions.append(h('button', { class: 'btn', disabled: busy, title: 'Translate the first 150 lines of one script and install them, to check the result in the game before paying for all of it.', onclick: () => run('test') }, 'Try 150 lines first'));
    if (p.installed) actions.append(h('button', { class: 'btn', onclick: launch }, 'Launch the game'));
    if (p.installed) actions.append(h('button', { class: 'btn btn-quiet danger', disabled: busy, onclick: () => run('restore') }, 'Remove the patch'));
    card.append(actions);
    card.append(h('p', { class: 'hint', text: p.installed
      ? 'The patch is installed. Removing it puts the original game files back.'
      : 'Yakusu saves as it goes: stop at any time and continue later. It only changes the game when it installs the patch, and that can be undone.' }));
    card.append(h('details', { class: 'advanced' }, h('summary', { text: 'Run one stage at a time' }),
      h('div', { class: 'row-actions' }, [['translate', 'Translate'], ['review', 'Review'], ['fit', 'Shorten overflowing lines'], ['glossary', 'Fix name spellings'], ['build', 'Rebuild and install']]
        .map(([a, t]) => h('button', { class: 'btn', onclick: () => run(a) }, t)))));
    box.replaceChildren(card);
    const mn = $('#manualNote');
    mn.replaceChildren();
    if (p.manual_output) {
      mn.append(h('div', { class: 'callout' }, 'This engine needs one manual step. The patched script files are in ',
        h('code', { text: p.manual_output }), '. ', (p.engine && p.engine.note) || '', ' ',
        h('button', { class: 'btn btn-quiet', onclick: () => safe(() => call('reveal', { which: 'manual' })) }, 'Open folder')));
    }
  }

  // 完訳 (a complete translation): shown once when a full run finishes.
  function showKept(result) {
    const key = S.job.finished;
    if (keptShownFor === key) return;
    keptShownFor = key;
    const name = S.project ? S.project.name : 'The game';
    const pet = h('div', { class: 'petals', 'aria-hidden': 'true' });
    const left = result.left || 0;
    $('#kept').replaceChildren(h('div', { class: 'kept', role: 'status' },
      pet, svgUse('branch'),
      h('h2', { lang: 'ja', text: '完訳' }),
      h('p', null, result.installed
        ? `${name} is in English: ${plural(result.applied || 0, 'line')} translated and installed. The original files are kept, so the patch can be removed at any time.`
        : `${name} is translated: ${plural(result.applied || 0, 'line')}. This engine needs one manual step to put them in the game (see below).`),
      left ? h('p', null, `${plural(left, 'line')} stayed in Japanese` + (result.declined ? ` (the model declined ${result.declined === left ? 'to translate them' : num(result.declined) + ' of them'})` : '') + '. ',
        h('a', { href: '#', onclick: (e) => { e.preventDefault(); linesState.filter = 'problems'; show('lines'); } }, 'Look at them')) : null,
      h('div', { class: 'inline' },
        result.installed ? h('button', { class: 'btn btn-primary', onclick: launch }, 'Launch the game') : null,
        h('button', { class: 'btn btn-quiet', onclick: () => $('#kept').replaceChildren() }, 'Dismiss'))));
    petals(pet, 9, true);
    show('home');
  }

  function launch() { safe(async () => { const r = await call('launch'); toast(`Started ${r.launched}.`); }); }
  const NEEDS_MODEL = ['auto', 'test', 'style', 'translate', 'review', 'fit', 'glossary'];
  function run(action) {
    safe(async () => {
      if (!S.project) throw new Error('Choose a game folder first.');
      if (NEEDS_MODEL.includes(action) && !translatorReady()) {
        const s = S.settings;
        return needSetup(s.keys[s.provider] ? 'model' : 'key', s.keys[s.provider] ? 'Choose a model first.' : 'Paste your API key first.');
      }
      if (action === 'restore' && !confirmInline('restore', 'Press again to remove the patch and put the original game files back.')) return;
      if (action === 'auto') $('#kept').replaceChildren();
      try {
        await call('run', { action });
      } catch (e) {
        // A local server that isn't running, or has nothing loaded.
        if (NEEDS_MODEL.includes(action) && S.settings.provider === 'local') return needSetup('model', e.message);
        throw e;
      }
      await refresh();
    });
  }
  let armed = null;
  function confirmInline(what, message) {
    // Ask by pressing twice instead of a blocking dialog.
    if (armed === what) { armed = null; return true; }
    armed = what;
    toast(message);
    setTimeout(() => { if (armed === what) armed = null; }, 4000);
    return false;
  }

  // ------------------------------------------------------------ style and names
  async function loadStyle() {
    if (!S || !S.project) return;
    const r = await call('get_style');
    $('#styleText').value = r.style || '';
    const names = Object.entries((r.glossary && r.glossary.names) || {});
    const speakers = ((S.project.stats || {}).speakers || []).map(([n]) => n);
    for (const sp of speakers) if (!names.find(([j]) => j === sp)) names.push([sp, '']);
    $('#namesBody').replaceChildren(...names.map(([j, e]) => pairRow(j, e)));
    $('#termsBody').replaceChildren(...(((r.glossary && r.glossary.terms) || []).map((t) => pairRow(t.jp, t.en))));
    $('#draftStyle').textContent = S.project.has_style ? 'Draft again from the script' : 'Draft now';
    $('#draftStyle').className = S.project.has_style ? 'btn' : 'btn btn-primary';
    $('#styleHint').textContent = S.project.has_style ? 'Drafting again replaces the style sheet and adds new names. Names you set are kept.' : 'Costs a few cents. Translating does this by itself if you skip it.';
  }
  function pairRow(j, e) {
    return h('tr', null, h('td', null, h('input', { value: j || '', 'aria-label': 'Japanese', lang: 'ja' })),
      h('td', null, h('input', { value: e || '', 'aria-label': 'English', placeholder: 'Not set yet' })));
  }
  $('#addName').addEventListener('click', () => { const r = pairRow('', ''); $('#namesBody').append(r); $('input', r).focus(); });
  $('#addTerm').addEventListener('click', () => { const r = pairRow('', ''); $('#termsBody').append(r); $('input', r).focus(); });
  $('#draftStyle').addEventListener('click', () => {
    if (S.project && S.project.has_style && !confirmInline('style', 'Press again to replace your style sheet and add any new names.')) return;
    run('style');
  });
  let styleTimer;
  function saveStyleSoon() {
    $('#styleSaved').textContent = 'Saving…';
    clearTimeout(styleTimer);
    styleTimer = setTimeout(() => safe(async () => {
      const pairs = (tb) => $$('tr', tb).map((tr) => $$('input', tr).map((i) => i.value.trim())).filter(([j]) => j);
      const names = {};
      for (const [j, e] of pairs($('#namesBody'))) names[j] = e;
      const terms = pairs($('#termsBody')).map(([jp, en]) => ({ jp, en }));
      await call('save_style', { style: $('#styleText').value, glossary: { names, terms } });
      $('#styleSaved').textContent = 'Saved. Every batch from now on follows these notes.';
    }), 700);
  }
  $('section[data-view="style"]').addEventListener('input', (e) => {
    if (e.target.matches('#styleText, .names input')) saveStyleSoon();
  });

  // ------------------------------------------------------------ text box
  function renderTest() {
    if (view !== 'test' || !S.project) return;
    const p = S.project;
    const sel = $('#testFile');
    if (document.activeElement !== sel) {
      const cur = p.test_file || '';
      sel.replaceChildren(h('option', { value: '', text: 'First scene (automatic)' }),
        ...(p.files || []).map((f) => h('option', { value: f, text: f.replace(/^(ks|scn)\//, '').replace(/\.json$/, ''), selected: f === cur })));
    }
    const box = p.box || { width: 56, lines: 3 };
    for (const [id, val, lab] of [['#boxWidth', box.width, '#widthVal'], ['#boxLines', box.lines, '#linesVal']]) {
      if (document.activeElement !== $(id)) $(id).value = val;
      $(lab).textContent = $(id).value;
    }
    const st = p.settings || {};
    if (document.activeElement !== $('#quotes')) $('#quotes').value = st.quotes || 'auto';
    $('#insertBreaks').checked = st.insert_breaks !== false;
    $('#boxHint').textContent = box.user_set ? 'Using your text box size.' :
      (box.mode === 'explicit' ? `Measured from the original script: lines of up to ${box.width} half-width characters, ${box.lines} lines per box.` :
        'This game wraps text itself. The width is an estimate; adjust it if words get split in the game.');
  }
  $('#testFile').addEventListener('change', (e) => safe(() => call('box', { test_file: e.target.value || null })));
  $('#runTest').addEventListener('click', () => run('test'));
  let boxTimer;
  function boxChanged() {
    $('#widthVal').textContent = $('#boxWidth').value;
    $('#linesVal').textContent = $('#boxLines').value;
    previewBox = { ...(previewBox || {}), width: +$('#boxWidth').value, lines: +$('#boxLines').value };
    clearTimeout(boxTimer);
    boxTimer = setTimeout(() => safe(async () => {
      await call('box', { width: +$('#boxWidth').value, lines: +$('#boxLines').value, quotes: $('#quotes').value, insert_breaks: $('#insertBreaks').checked });
      await loadPreview();
    }), 220);
  }
  ['#boxWidth', '#boxLines'].forEach((id) => $(id).addEventListener('input', boxChanged));
  ['#quotes', '#insertBreaks'].forEach((id) => $(id).addEventListener('change', boxChanged));
  $('#applyFit').addEventListener('click', () => run('refit'));
  $('#launch1').addEventListener('click', launch);
  $('#restore1').addEventListener('click', () => run('restore'));
  $('#prevLine').addEventListener('click', () => { previewIdx = Math.max(0, previewIdx - 1); drawPreview(); });
  $('#nextLine').addEventListener('click', () => { previewIdx = Math.min(previewLines.length - 1, previewIdx + 1); drawPreview(); });
  $('#scene').addEventListener('keydown', (e) => {
    if (e.key === 'ArrowLeft') $('#prevLine').click();
    if (e.key === 'ArrowRight') $('#nextLine').click();
  });

  async function loadPreview() {
    if (!S || !S.project) return;
    try { const r = await call('preview'); previewLines = r.lines || []; previewBox = r.box; } catch (e) { previewLines = []; }
    previewIdx = Math.min(previewIdx, Math.max(0, previewLines.length - 1));
    drawPreview();
  }
  function drawPreview() {
    const tb = $('#tbText');
    const box = previewBox || (S && S.project && S.project.box) || { width: 56, lines: 3 };
    // Size the font so 'width' half-width characters fill the box.
    const inner = $('#textbox').clientWidth * 0.94;
    tb.style.fontSize = Math.max(9, Math.min(30, inner / (box.width * 0.6))) + 'px';
    tb.style.minHeight = `calc(${box.lines} * 1.6em)`;
    const ln = previewLines[previewIdx];
    $('#prevLine').disabled = previewIdx <= 0;
    $('#nextLine').disabled = previewIdx >= previewLines.length - 1;
    if (!ln) {
      $('#nameplate').textContent = '';
      tb.replaceChildren(h('span', { class: 'row empty', text: 'Translated lines appear here, wrapped exactly as the game will show them.' }));
      $('#tbOver').textContent = '';
      $('#lineCounter').textContent = '';
      $('#sceneJp').textContent = '';
      return;
    }
    $('#nameplate').textContent = ln.name || '';
    tb.replaceChildren(...ln.lines.map((r, i) => h('span', { class: 'row' + (i >= box.lines ? ' overflow' : ''), text: r || ' ' })));
    $('#tbOver').textContent = ln.over ? `${plural(ln.over, 'line')} too long` : '';
    $('#lineCounter').textContent = `${previewIdx + 1} / ${previewLines.length}`;
    $('#sceneJp').textContent = (ln.jp || '').replace(/⟦\d+⟧/g, ' ');
  }
  window.addEventListener('resize', () => { if (view === 'test') drawPreview(); });

  // ------------------------------------------------------------ lines
  $$('#lineFilter button').forEach((b) => b.addEventListener('click', () => {
    linesState.filter = b.dataset.f; linesState.page = 0; loadLines();
  }));
  let qTimer;
  $('#lineSearch').addEventListener('input', (e) => { clearTimeout(qTimer); qTimer = setTimeout(() => { linesState.q = e.target.value; linesState.page = 0; loadLines(); }, 250); });

  let lastRows = [];
  $('#retranslatePage').addEventListener('click', () => safe(async () => {
    await call('retranslate', { ids: lastRows.filter((r) => !r.skip).map((r) => r.id) });
    refresh();
  }));
  $('#exportCsv').addEventListener('click', () => safe(async () => {
    const r = await call('export_csv');
    toast(`Saved ${String(r.path).split(/[\\/]/).pop()} in the project's exports folder.`);
  }));
  $('#importCsv').addEventListener('change', (e) => safe(async () => {
    const f = e.target.files[0];
    e.target.value = '';
    if (!f) return;
    const r = await call('import_csv', { text: await f.text() });
    toast(`Imported ${plural(r.changed, 'edited line')}` + (r.problems ? `; ${r.problems} need a look` : '') +
      (r.unknown ? `; ${plural(r.unknown, 'row')} didn't match this game` : '') + '. Rebuild to put them in the game.');
    loadLines();
  }));

  async function loadLines() {
    $$('#lineFilter button').forEach((b) => b.setAttribute('aria-pressed', String(b.dataset.f === linesState.filter)));
    $('#retranslatePage').hidden = !['problems', 'untranslated', 'reviewed'].includes(linesState.filter);
    const list = $('#linesList');
    if (!S || !S.project || !S.project.extracted) {
      list.replaceChildren(h('div', { class: 'empty-state' }, svgUse('branch'), 'Open a game on the Translate page. Its lines will be listed here.'));
      $('#pager').replaceChildren();
      return;
    }
    const r = await call('lines', { filter: linesState.filter, q: linesState.q, page: linesState.page });
    lastRows = r.rows;
    const nr = r.rows.filter((x) => !x.skip).length;
    $('#retranslatePage').textContent = nr === 1 ? 'Retranslate this line' : `Retranslate these ${num(nr)} lines`;
    $('#retranslatePage').disabled = !nr;
    list.replaceChildren(...(r.rows.length ? r.rows.map(lineRow) : [h('div', { class: 'empty-state', text: linesState.q ? 'No lines match that search.' : 'Nothing here.' })]));
    const pages = Math.max(1, Math.ceil(r.total / r.per));
    $('#pager').replaceChildren(h('span', { text: plural(r.total, 'line') }),
      h('button', { class: 'btn btn-quiet', disabled: r.page <= 0, onclick: () => { linesState.page--; loadLines(); } }, 'Previous'),
      h('span', { text: `Page ${r.page + 1} of ${pages}` }),
      h('button', { class: 'btn btn-quiet', disabled: r.page + 1 >= pages, onclick: () => { linesState.page++; loadLines(); } }, 'Next'));
  }
  const STAGE_NAME = { translate: 'Translated', review: 'Improved in review', reviewed: 'Reviewed', fit: 'Shortened to fit', manual: 'Edited by you', cleared: 'Queued to translate again' };
  function lineRow(r) {
    const ta = h('textarea', { 'aria-label': 'English', placeholder: r.skip ? '' : 'Not translated yet' });
    ta.value = r.en || '';
    const meta = h('div', { class: 'meta' });
    if (STAGE_NAME[r.stage]) meta.append(h('span', { text: STAGE_NAME[r.stage] }));
    if (r.problems && r.problems.length) meta.append(h('span', { class: 'bad', text: cap(r.problems.join(', ')) }));
    if (r.glossary && r.glossary.length) meta.append(h('span', { class: 'gloss', text: `Should use ${r.glossary.join(', ')}` }));
    if (r.skip) meta.append(h('span', { text: `Skipped: ${r.skip}` }));
    const before = r.before && r.before !== r.en ? h('div', { class: 'before', title: 'Before review', text: r.before }) : null;
    ta.addEventListener('change', () => safe(async () => {
      const res = await call('line', { id: r.id, en: ta.value });
      meta.replaceChildren(res.problems && res.problems.length
        ? h('span', { class: 'bad', text: `Saved, but check it: ${res.problems.join(', ')}` })
        : h('span', { text: ta.value.trim() ? 'Saved. Rebuild the patch to put it in the game.' : 'Cleared. It will be translated again on the next run.' }));
    }));
    return h('div', { class: 'ln' + (r.skip ? ' skipped' : '') },
      h('div', { class: 'who', title: r.file, text: r.who || 'Narration' }),
      h('div', { class: 'src', lang: 'ja', text: r.jp }),
      h('div', null, ta, before, meta),
      h('div', { class: 'actions' },
        r.skip ? null : h('button', { class: 'mini', title: 'Translate this line again', 'aria-label': 'Translate this line again',
          onclick: () => safe(async () => { await call('retranslate', { ids: [r.id] }); refresh(); }) }, '↻'),
        h('button', { class: 'mini', title: r.skip ? 'Translate this line' : 'Don\'t translate this line', 'aria-label': r.skip ? 'Translate this line' : 'Skip this line',
          onclick: () => safe(async () => { await call('line', { id: r.id, skip: !r.skip }); loadLines(); }) }, r.skip ? '↺' : '×')));
  }

  // ------------------------------------------------------------ log
  function renderLog() {
    const el = $('#log');
    const atBottom = el.scrollTop + el.clientHeight >= el.scrollHeight - 20;
    if (!(S && S.logs.length)) { el.textContent = 'Nothing yet.'; return; }
    el.replaceChildren(...S.logs.map((l) => h('div', { class: /^(Error|Unexpected)|failed|Couldn/.test(l.m) ? 'err' : '' },
      h('span', { class: 't', text: new Date(l.t * 1000).toLocaleTimeString() }), l.m)));
    if (atBottom) el.scrollTop = el.scrollHeight;
  }

  // ------------------------------------------------------------ start
  refresh().then(() => {
    show('home');
    loadModels();
    // A game that was opened but never read (the app was closed first): read it now.
    const p = S && S.project;
    if (p && !p.extracted && p.engine && p.engine.engine_id !== 'unsupported' && !(S.job || {}).running) {
      call('run', { action: 'extract' }).then(refresh, () => {});
    }
    loop();
  });
})();
