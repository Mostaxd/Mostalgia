// Episode 1 · Press record. The HTML holds the finished scene (what you see
// without JS or with reduced motion); here it is rewound and replayed on a
// clock that only runs while the scene is on screen.
(() => {
  const site = window.MostalgiaSite;
  const ep = document.getElementById('ep1');
  const scene = ep && ep.querySelector('#e1Scene');
  if (!site || !scene || site.reduce) return;
  const { $, $$, sprite, type, preload } = site;

  const timer = $('#e1Timer'), state = $('#e1State'), pill = $('#e1Pill'), callStat = $('#e1CallStat');
  const toast = $('.e1-toast', scene), toastMsg = $('#e1ToastMsg'), recBtn = $('#e1RecBtn');
  const disk = $('.e1-disk', scene), impState = $('#e1ImpState'), impBar = $('#e1ImpBar');
  const mori = $('#e1Mori'), say = $('#e1Say'), replay = $('#e1Replay');
  const tiles = { lena: $('.e1-tile.lena', scene), sam: $('.e1-tile.sam', scene), you: $('.e1-tile.you', scene) };
  const waves = { me: $$('#e1WaveMe i'), others: $$('#e1WaveOthers i') };
  const points = $$('.e1-points .step', ep);
  const finalSay = say.textContent, toastAsk = toastMsg.textContent;
  const STATES = ['s-mic', 's-call', 's-toast', 's-auto', 's-app', 's-rec', 's-quiet', 's-nudge', 's-later', 's-flicker', 's-off', 's-dead', 's-stamp', 's-drag', 's-drop', 's-import'];
  const clock = (s) => `${String(Math.floor(s / 60)).padStart(2, '0')}:${String(Math.floor(s % 60)).padStart(2, '0')}`;
  const on = (...c) => scene.classList.add(...c);
  const off = (...c) => scene.classList.remove(...c);

  ep.classList.add('e1-js');

  const pose = (p, line) => {
    const src = sprite('mori', p);
    if (mori.getAttribute('src') !== src) { mori.src = src; mori.classList.remove('hop'); void mori.offsetWidth; mori.classList.add('hop'); }
    if (line) type(say, line);
  };
  // a beat that matches one of the bullets: highlight it, and let the page's
  // companion (the corner guide on phones) say the same line
  const beat = (i) => {
    const step = points[i];
    points.forEach((s) => s.classList.toggle('on', s === step));
    if (ep._activate) ep._activate(step);
    pose(step.dataset.pose, step.dataset.say);
  };

  let events = [], next = 0, t = 0, last = 0, raf = 0, waveT = 0;
  let started = false, visible = false;
  // recording: null when the recorder is not running; otherwise the timer reads base + (t - from)
  let rec = null, talker = null, quiet = false, shown = '', tickAt = -1;
  const levels = { me: [], others: [] };

  const setWave = (k, fill) => {
    levels[k] = waves[k].map(() => fill);
    waves[k].forEach((b) => { b.style.transform = `scaleY(${fill})`; });
  };
  const talk = (who) => {
    talker = who;
    Object.entries(tiles).forEach(([n, el]) => el.classList.toggle('talk', n === who));
  };

  const reset = () => {
    off(...STATES, 'e1-paused');
    on('e1-anim');
    toast.classList.remove('done');
    recBtn.classList.remove('press');
    toastMsg.textContent = toastAsk;
    callStat.textContent = 'Joining…';
    pill.textContent = '+ New recording';
    timer.textContent = shown = '00:00';
    state.textContent = 'Ready';
    impState.textContent = 'Importing…';
    impBar.style.width = '0%';
    disk.classList.remove('tick');
    points.forEach((s) => s.classList.remove('on'));
    setWave('me', 0.05); setWave('others', 0.05);
    talk(null);
    rec = null; quiet = false; tickAt = -1; t = 0; next = 0; waveT = 0;
    mori.src = sprite('mori', 'idle');
    say.textContent = 'Quiet desk. Waiting for a call.';

    const ev = [];
    const at = (time, fn) => ev.push({ time, fn });
    // the call starts and grabs the mic
    at(0.5, () => { callStat.textContent = 'Connecting…'; });
    at(1.0, () => { on('s-call', 's-mic'); callStat.textContent = 'In call'; pose('listening', 'Your call app just grabbed the mic…'); });
    // Mostalgia offers to record, and with auto-start on it just does
    at(1.5, () => { on('s-toast'); beat(0); });
    at(1.7, () => on('s-auto'));
    at(2.9, () => recBtn.classList.add('press'));
    at(3.2, () => {
      recBtn.classList.remove('press');
      toast.classList.add('done');
      toastMsg.textContent = 'Recording started. It stops when the call ends.';
      on('s-app', 's-rec');
      pill.textContent = 'Recording';
      state.textContent = 'Recording';
      rec = { base: 0, from: 3.2 };
    });
    at(4.5, () => off('s-toast'));
    // two tracks: whoever talks lights up their own waveform
    at(3.6, () => { talk('lena'); beat(1); });
    at(4.7, () => talk(null));
    at(4.8, () => talk('you'));
    at(5.7, () => talk(null));
    at(5.8, () => talk('sam'));
    at(6.5, () => talk(null));
    // you keep talking, but the mic has gone silent
    at(6.7, () => { quiet = true; on('s-quiet'); talk('you'); });
    at(7.2, () => { on('s-nudge'); beat(2); });
    at(8.5, () => { off('s-nudge', 's-quiet'); quiet = false; });
    at(9.1, () => { talk(null); on('s-later'); rec = { base: 870.2, from: 9.1 }; });
    at(9.25, () => talk('lena'));
    // power cut
    at(10.1, () => { off('s-later'); on('s-flicker'); talk(null); beat(3); });
    at(11.0, () => {
      on('s-off', 's-dead');
      off('s-flicker', 's-rec', 's-mic');
      rec = null;
      timer.textContent = shown = '14:32';
    });
    // restart: the tracks are repaired at launch
    at(12.2, () => {
      off('s-off', 's-dead');
      callStat.textContent = 'Rejoined';
      on('s-mic');
      pill.textContent = '+ New recording';
      state.textContent = 'Repairing tracks…';
      setWave('me', 0.05); setWave('others', 0.05);
      waves.me.concat(waves.others).forEach((b) => { b.style.transform = ''; });
    });
    at(12.8, () => {
      on('s-stamp');
      state.textContent = 'Recovered';
      pose('talking', 'Recovered: 14:32 of audio. You lost seconds, not the meeting.');
    });
    // an old recording is dropped in and becomes a meeting
    at(14.3, () => { on('s-drag'); beat(4); });
    at(15.4, () => { on('s-drop', 's-import'); });
    at(16.4, () => { impState.textContent = 'Imported · 42 min'; });
    at(16.8, () => { off('s-drag'); points.forEach((s) => s.classList.remove('on')); pose('idle', finalSay); });
    at(17.3, () => { replay.hidden = false; });
    events = ev.sort((x, y) => x.time - y.time);
  };

  // waveforms scroll right to left; the live track bounces, the other idles near the floor
  const sample = (k) => {
    if (k === 'me' && quiet) return 0.03;
    const live = k === 'me' ? talker === 'you' : talker === 'lena' || talker === 'sam';
    return live ? 0.3 + 0.68 * Math.random() * Math.abs(Math.sin(t * 9 + (k === 'me' ? 1 : 3))) : 0.04 + 0.06 * Math.random();
  };
  const frame = (now) => {
    raf = 0;
    const dt = Math.min(0.1, (now - last) / 1000);
    t += dt;
    last = now;
    while (next < events.length && events[next].time <= t) events[next++].fn();
    if (rec) {
      const s = rec.base + (t - rec.from);
      const c = clock(s);
      if (c !== shown) timer.textContent = shown = c;
      // synced to disk every 2 seconds
      const k = Math.floor(s / 2);
      if (k !== tickAt) { tickAt = k; disk.classList.add('tick'); }
      else if (s % 2 > 0.5) disk.classList.remove('tick');
      waveT += dt;
      if (waveT > 0.07) {
        waveT = 0;
        for (const key of ['me', 'others']) {
          const l = levels[key];
          l.shift();
          l.push(sample(key));
          waves[key].forEach((b, i) => { b.style.transform = `scaleY(${l[i].toFixed(3)})`; });
        }
      }
    }
    if (t >= 15.4 && t < 16.6) impBar.style.width = `${(Math.min(1, (t - 15.4) / 1.0) * 100).toFixed(1)}%`;
    if (visible && next < events.length) raf = requestAnimationFrame(frame);
  };
  const play = () => {
    scene.classList.remove('e1-paused');
    if (!raf && next < events.length) { last = performance.now(); raf = requestAnimationFrame(frame); }
  };
  const pause = () => { cancelAnimationFrame(raf); raf = 0; scene.classList.add('e1-paused'); };

  reset();
  ['listening', 'talking', 'thinking', 'error', 'writing'].forEach((p) => preload(sprite('mori', p)));
  // start once ~40% of the scene (or of the screen, on tall phones) is in view; pause whenever it leaves
  new IntersectionObserver(([e]) => {
    const vh = e.rootBounds ? e.rootBounds.height : innerHeight;
    const seen = e.intersectionRect.height / Math.min(e.boundingClientRect.height, vh);
    visible = started ? e.isIntersecting : seen >= 0.4;
    if (visible) { started = true; play(); } else pause();
  }, { threshold: [0, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 1] }).observe(scene);

  replay.addEventListener('click', () => { pause(); reset(); started = visible = true; play(); });
})();
