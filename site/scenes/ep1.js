// Episode 1 · Press record. The HTML holds the finished scene (what you see
// without JS or with reduced motion); here it is rewound and its beats are
// handed to the shared pacing clock (pacing.js), which plays them chapter by
// chapter with a caption, only while the scene is on screen.
(() => {
  const site = window.MostalgiaSite;
  const ep = document.getElementById('ep1');
  const scene = ep && ep.querySelector('#e1Scene');
  if (!site || !scene || !site.timeline) return;
  const { $, $$, sprite, type, preload } = site;

  // chapters in scene seconds; focus = what stays lit while the rest dims
  const CHAPTERS = [
    { id: 'auto', label: 'Auto-start', start: 0,
      caption: 'Your call app grabs the mic, so Mostalgia starts recording by itself.',
      focus: ['.e1-panel', '.e1-call', '.e1-toast', '.e1-side', '.e1-bar'] },
    { id: 'tracks', label: 'Two tracks', start: 3.6,
      caption: 'Your voice goes on the Me track, everyone else on Others. Each lights up when that side talks.',
      focus: ['.e1-call', '.e1-tracks'] },
    { id: 'nudge', label: 'Mic nudge', start: 6.7, holds: [7.8],
      caption: 'You keep talking but your mic picks up nothing, so Mostalgia asks: muted by accident?',
      focus: ['.e1-call', '.e1-tracks'] },
    { id: 'crash', label: 'Crash-safe', start: 9.1, holds: [11.6],
      caption: '14 minutes in, the power cuts out. On restart the tracks are repaired: only seconds are lost.',
      focus: ['.e1-panel', '.e1-call', '.e1-bar', '.e1-tracks', '.e1-status', '.e1-stamp'] },
    { id: 'import', label: 'Import', start: 14.3,
      caption: 'Drop in a recording you already have, and it becomes a meeting like any other.',
      focus: ['.e1-side'] },
  ];

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


  const pose = (p, line) => {
    const src = sprite('mori', p);
    if (mori.getAttribute('src') !== src) { mori.src = src; mori.classList.remove('hop'); void mori.offsetWidth; mori.classList.add('hop'); }
    if (line) type(say, line);
  };
  // a beat that matches one of the bullets: highlight it, and let the page's
  // companion (the corner guide on phones) say the same line
  const beat = (i) => {
    const step = points[i];
    if (ep._activate) ep._activate(step);
    pose(step.dataset.pose, step.dataset.say);
  };

  // amb: real seconds while playing (keeps ticking through the holds, so the
  // recording timer and the waveforms never freeze)
  let amb = 0, waveT = 0;
  // recording: null when the recorder is not running; otherwise the timer reads base + (amb - from)
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
    off(...STATES);
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
    setWave('me', 0.05); setWave('others', 0.05);
    talk(null);
    rec = null; quiet = false; tickAt = -1; waveT = 0;
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
    at(3.2, (late) => {
      recBtn.classList.remove('press');
      toast.classList.add('done');
      toastMsg.textContent = 'Recording started. It stops when the call ends.';
      on('s-app', 's-rec');
      pill.textContent = 'Recording';
      state.textContent = 'Recording';
      rec = { base: late, from: amb };
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
    at(9.1, (late) => { talk(null); on('s-later'); rec = { base: 870.2 + late, from: amb }; });
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
    at(16.8, () => { off('s-drag'); pose('idle', finalSay); });
    return ev;
  };

  // waveforms scroll right to left; the live track bounces, the other idles near the floor
  const sample = (k) => {
    if (k === 'me' && quiet) return 0.03;
    const live = k === 'me' ? talker === 'you' : talker === 'lena' || talker === 'sam';
    return live ? 0.3 + 0.68 * Math.random() * Math.abs(Math.sin(amb * 9 + (k === 'me' ? 1 : 3))) : 0.04 + 0.06 * Math.random();
  };
  const tick = (t, dt) => {
    amb += dt;
    if (rec) {
      const s = rec.base + (amb - rec.from);
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
    const w = t < 15.4 ? '0%' : `${(Math.min(1, (t - 15.4) / 1.0) * 100).toFixed(1)}%`;
    if (impBar.style.width !== w) impBar.style.width = w;
  };

  if (!site.reduce) ['listening', 'talking', 'thinking', 'error', 'writing'].forEach((p) => preload(sprite('mori', p)));
  site.timeline(scene, CHAPTERS, {
    bar: $('.e1-points', ep),
    ctlBefore: $('.e1-stagebox', ep),
    parts: '.e1-panel, .e1-call, .e1-toast, .e1-side, .e1-bar, .e1-tracks, .e1-status, .e1-stamp',
    dim: 'brightness(.5) saturate(.55)',
    replay,
    end: 17.3,
    reset,
    tick,
  });
})();
