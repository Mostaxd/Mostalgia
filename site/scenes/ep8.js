// Episode 8 · Privacy: where your data goes.
// The radio switch drives the diagram through CSS alone (so it works without
// JS and with reduced motion). Here Jin reacts to it, packets travel along
// the open lanes, and on first view the scene plays itself once: Local →
// Hybrid → Cloud, then auto cleanup and forgetting a meeting.
(() => {
  const site = window.MostalgiaSite;
  if (!site) return;
  const { reduce, $, $$, sprite, type, preload } = site;
  const box = $('#ep8Box');
  if (!box) return;
  const scene = $('#ep8Scene', box);
  const radios = $$('input[name=ep8-mode]', box);
  const jin = $('#ep8Jin'), say = $('#ep8Say'), replay = $('#ep8Replay');
  const lanes = { text: $('.ep8-lane.text', scene), audio: $('.ep8-lane.audio', scene) };
  const tracks = { text: $('.ep8-track', lanes.text), audio: $('.ep8-track', lanes.audio) };
  const live = $('.ep8-row.now', scene);
  const chips = { text: $('.ep8-chip.tx', live), audio: $('.ep8-chip.au', live) };
  const rx = { text: $('.ep8-rx .tx', scene), audio: $('.ep8-rx .au', scene) };
  const oldRow = $('.ep8-row.old', scene), keptRow = $('.ep8-row.kept', scene), forgetRow = $('.ep8-row.forget', scene);
  const dialog = $('.ep8-dialog', scene), check = $('.ep8-check', scene), del = $('.ep8-dbtns .del', scene);
  const index = $('.ep8-store.idx', scene);
  const finalSay = say.textContent;

  const MODES = {
    local: { pose: 'idle', line: 'Local. Nothing leaves this laptop. Locked.' },
    hybrid: { pose: 'thinking', line: 'Hybrid. Text goes out for summaries and Ask. Audio stays home.' },
    cloud: { pose: 'talking', line: 'Cloud. Audio goes out to be transcribed. Your call, and the badge says so.' },
  };
  // which lanes are open in each mode
  const OPEN = { local: { text: false, audio: false }, hybrid: { text: true, audio: false }, cloud: { text: true, audio: true } };
  const current = () => (radios.find((r) => r.checked) || radios[1]).value;

  const pose = (p, line) => {
    const src = sprite('jin', p);
    if (jin.getAttribute('src') !== src) {
      jin.src = src;
      if (!reduce) { jin.classList.remove('hop'); void jin.offsetWidth; jin.classList.add('hop'); }
    }
    if (line) type(say, line);
  };
  const restart = (el, cls) => { el.classList.remove(cls); void el.offsetWidth; el.classList.add(cls); };

  // a lane that just closed gets its lock "click"
  let wasOpen = { ...OPEN[current()] };
  const clackClosed = (open) => {
    for (const k of ['text', 'audio']) if (wasOpen[k] && !open[k] && !reduce) restart(lanes[k], 'clack');
    wasOpen = { ...open };
  };
  const setMode = (m, speak = true) => {
    const r = radios.find((x) => x.value === m);
    if (r) r.checked = true;
    clackClosed(OPEN[m]);
    if (speak) pose(MODES[m].pose, MODES[m].line);
  };

  // the visitor takes the switch: always works, also with reduced motion
  let userTook = false;
  radios.forEach((r) => r.addEventListener('change', () => {
    if (!r.checked) return;
    userTook = true;
    box.classList.remove('ep8-pre');
    clackClosed(OPEN[r.value]);
    pose(MODES[r.value].pose, MODES[r.value].line);
  }));
  ['idle', 'thinking', 'talking', 'writing'].forEach((p) => preload(sprite('jin', p)));
  if (reduce) return;

  // ───────── animated story ─────────
  scene.classList.add('ep8-anim');
  let events = [], next = 0, t = 0, last = 0, raf = 0, started = false, visible = false;
  const spawnAt = { text: 0, audio: 0 };

  const packet = (lane) => {
    const open = OPEN[current()][lane] && !box.classList.contains('ep8-pre');
    const pk = document.createElement('span');
    pk.className = `ep8-pk ${lane === 'text' ? 'tx' : 'au'} ${open ? 'fly' : 'bump'}`;
    if (open) pk.style.setProperty('--dur', `${lane === 'text' ? 2.6 : 2.1}s`);
    pk.addEventListener('animationend', () => {
      pk.remove();
      if (open) { rx[lane].classList.add('got'); clearTimeout(rx[lane]._t); rx[lane]._t = setTimeout(() => rx[lane].classList.remove('got'), 260); }
    });
    tracks[lane].append(pk);
    restart(chips[lane], 'ping');
  };
  // how often a packet leaves, per lane: open lanes stream, a locked audio lane in Hybrid shows audio bouncing back
  const rate = (lane) => {
    const m = current();
    if (box.classList.contains('ep8-pre') || m === 'local') return 0;
    if (OPEN[m][lane]) return lane === 'text' ? 0.8 : 0.55;
    return lane === 'audio' ? 2.4 : 0;
  };

  const reset = () => {
    $$('.ep8-pk:not(.st)', scene).forEach((p) => p.remove());
    oldRow.classList.remove('cleaned', 'dissolve');
    keptRow.classList.remove('spared');
    forgetRow.classList.remove('gone', 'crumple');
    dialog.classList.remove('on');
    check.classList.remove('tick');
    del.classList.remove('press');
    index.classList.remove('pulse');
    scene.classList.remove('spot-clean', 'spot-forget');
    Object.values(lanes).forEach((l) => l.classList.remove('clack'));
    replay.hidden = true;
    userTook = false;
    box.classList.add('ep8-pre');
    const r = radios.find((x) => x.value === 'local');
    r.checked = true;
    wasOpen = { text: true, audio: true };
    jin.src = sprite('jin', 'idle');
    say.textContent = 'Where does your meeting go? Watch the lanes.';
    t = 0; next = 0; spawnAt.text = 0.2; spawnAt.audio = 0.5;

    const ev = [];
    const at = (time, fn, mode) => ev.push({ time, fn, mode });
    // 1 · Local: both locks click shut, the cloud goes quiet
    at(0.6, () => pose('idle', MODES.local.line), true);
    at(1.4, () => { box.classList.remove('ep8-pre'); clackClosed(OPEN.local); }, true);
    // 2 · Hybrid: text travels, audio bounces off its lock
    at(5.2, () => setMode('hybrid'), true);
    // 3 · Cloud: audio travels too
    at(10.8, () => setMode('cloud'), true);
    // back to the default before the closing beats
    at(16.2, () => setMode('hybrid', false), true);
    // 4 · Auto cleanup
    at(16.2, () => { scene.classList.add('spot-clean'); pose('writing', 'Old audio, cut after 30 days. The transcript stays.'); });
    at(17.4, () => oldRow.classList.add('dissolve'));
    at(18.4, () => oldRow.classList.add('cleaned'));
    at(19.0, () => keptRow.classList.add('spared'));
    // 5 · Forget a meeting, completely
    at(21.6, () => { scene.classList.replace('spot-clean', 'spot-forget'); pose('talking', 'Want a meeting gone? Gone. Index and all.'); });
    at(22.2, () => dialog.classList.add('on'));
    at(23.2, () => check.classList.add('tick'));
    at(24.0, () => del.classList.add('press'));
    at(24.3, () => { dialog.classList.remove('on'); forgetRow.classList.add('crumple'); });
    at(24.8, () => index.classList.add('pulse'));
    at(25.6, () => { forgetRow.classList.remove('crumple'); forgetRow.classList.add('gone'); });
    at(27.0, () => { scene.classList.remove('spot-forget'); pose('idle', finalSay); });
    at(27.6, () => { replay.hidden = false; });
    events = ev.sort((a, b) => a.time - b.time);
  };

  const frame = (now) => {
    raf = 0;
    t += Math.min(0.1, (now - last) / 1000);
    last = now;
    while (next < events.length && events[next].time <= t) {
      const e = events[next++];
      if (!(e.mode && userTook)) e.fn();
    }
    for (const lane of ['text', 'audio']) {
      const r = rate(lane);
      if (!r) { spawnAt[lane] = Math.max(spawnAt[lane], t + 0.3); continue; }
      if (t >= spawnAt[lane]) { packet(lane); spawnAt[lane] = t + r * (0.8 + Math.random() * 0.4); }
    }
    if (visible) raf = requestAnimationFrame(frame);
  };
  const play = () => {
    scene.classList.remove('ep8-paused');
    if (!raf) { last = performance.now(); raf = requestAnimationFrame(frame); }
  };
  const pause = () => { cancelAnimationFrame(raf); raf = 0; scene.classList.add('ep8-paused'); };

  reset();
  pause();
  // start once ~40% of the scene (or of the screen, on tall phones) is in view; pause whenever it leaves
  new IntersectionObserver(([e]) => {
    const vh = e.rootBounds ? e.rootBounds.height : innerHeight;
    const seen = e.intersectionRect.height / Math.min(e.boundingClientRect.height, vh);
    visible = started ? e.isIntersecting : seen >= 0.4;
    if (visible) { started = true; play(); } else pause();
  }, { threshold: [0, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 1] }).observe(scene);

  replay.addEventListener('click', () => {
    pause();
    reset();
    started = visible = true;
    (radios.find((r) => r.checked) || radios[0]).focus({ preventScroll: true });
    if (scene.getBoundingClientRect().top < 0) scene.scrollIntoView({ behavior: 'smooth', block: 'center' });
    play();
  });
})();
