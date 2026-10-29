// Episode 3: who said it? The grey "Others" track splits into three voices,
// one wrong line is moved, Speaker 2 is renamed to Lena everywhere at once,
// her voice print is kept, and on Thursday she is recognized by voice.
// The HTML holds the finished state (no JS, reduced motion); here it is
// rewound and replayed on a clock that only runs while the scene is on screen.
(() => {
  const site = window.MostalgiaSite;
  const scene = document.getElementById('e3Scene');
  if (!site || !scene || site.reduce) return;
  const { $, $$, sprite, type, preload } = site;

  const win = $('.e3-win', scene);
  const slot = $('#e3Slot'), input = $('#e3Input'), save = $('#e3Save');
  const menu = $('#e3Menu'), pick = $('#e3MenuPick'), cursor = $('#e3Cursor');
  const count = $('#e3Count'), jin = $('#e3Jin'), say = $('#e3Say');
  const print = $('#e3Print'), next = $('#e3Next'), nTimer = $('#e3NTimer'), recog = $('#e3Recog');
  const nLine = $('.e3-nline', next), nChip = $('#e3NChip'), nText = $('#e3NText');
  const replay = $('#e3Replay');
  const laneChip = (k) => $(`.e3-lane.${k} .e3-chip`, scene);
  const lines = $$('.e3-line', scene).map((li) => ({
    li, chip: $('.e3-chip', li), spk: li.dataset.spk, first: li.dataset.first || li.dataset.spk,
  }));
  const finalSay = say.textContent, finalNText = nText.textContent, finalCount = count.textContent;
  const LABEL = { me: 'Me', s1: 'Speaker 1', s2: 'Speaker 2', s3: 'Speaker 3', oth: 'Others' };

  const setChip = (chip, cls, text, fx) => {
    chip.classList.remove('me', 's1', 's2', 's3', 'oth', 'pop', 'ripple');
    chip.classList.add(cls);
    chip.textContent = text;
    if (fx) { void chip.offsetWidth; chip.classList.add(fx); }
  };
  const pose = (p, line) => {
    const src = sprite('jin', p);
    if (jin.getAttribute('src') !== src) { jin.src = src; jin.classList.remove('hop'); void jin.offsetWidth; jin.classList.add('hop'); }
    if (line) type(say, line);
  };
  // the cursor glides to the middle of an element (its tip lands slightly inside)
  const aim = (el, dx = 0.5, dy = 0.55) => {
    const s = win.getBoundingClientRect(), r = el.getBoundingClientRect();
    cursor.style.transform = `translate(${r.left - s.left + r.width * dx}px, ${r.top - s.top + r.height * dy}px)`;
  };
  const click = () => { cursor.classList.remove('click'); void cursor.offsetWidth; cursor.classList.add('click'); };
  const placeMenu = (el) => {
    const w = win.getBoundingClientRect(), r = el.getBoundingClientRect();
    const left = Math.min(r.left - w.left + 8, w.width - menu.offsetWidth - 8);
    menu.style.transform = '';
    menu.style.left = `${Math.max(8, left)}px`;
    menu.style.top = `${r.bottom - w.top + 4}px`;
  };

  // sparkles around Lena's chip when she is recognized
  const sparkle = () => {
    $$('.e3-spark', nLine).forEach((s) => s.remove());
    [['-28%', '-90%', '16px', '0s'], ['78%', '-100%', '20px', '.08s'], ['96%', '30%', '13px', '.18s'], ['-20%', '50%', '12px', '.24s', '#fff'], ['36%', '-120%', '11px', '.3s', '#fff']]
      .forEach(([x, y, s, d, c]) => {
        const sp = document.createElement('i');
        sp.className = 'e3-spark';
        sp.style.cssText = `--x:${x};--y:${y};--s:${s};--d:${d}${c ? `;--c:${c}` : ''}`;
        nChip.append(sp);
      });
    nChip.classList.remove('kira'); void nChip.offsetWidth; nChip.classList.add('kira');
  };

  let events = [], cur = 0, t = 0, last = 0, raf = 0, started = false, visible = false;

  const reset = () => {
    scene.classList.add('e3-anim', 'e3-merged', 'e3-wrong');
    scene.classList.remove('e3-scanning', 'e3-cut');
    setChip(laneChip('s1'), 'oth', 'Others');
    setChip(laneChip('s2'), 's2', 'Speaker 2');
    lines.forEach((l) => { setChip(l.chip, l.spk === 'me' ? 'me' : 'oth', l.spk === 'me' ? 'Me' : 'Others'); l.li.classList.remove('hl'); });
    count.textContent = '2 speakers';
    slot.dataset.s = 'scan';
    input.textContent = '';
    input.classList.remove('focus');
    save.classList.remove('press');
    menu.classList.remove('on');
    pick.classList.remove('on');
    cursor.classList.remove('on', 'click');
    print.classList.remove('in');
    next.classList.remove('in', 'slam', 'rec');
    nLine.classList.remove('on', 'prov');
    recog.classList.remove('in');
    nChip.classList.remove('kira');
    setChip(nChip, 'oth', '…');
    nText.textContent = '';
    nTimer.textContent = '00:00';
    jin.src = sprite('jin', 'listening');
    say.textContent = 'Two tracks. You, and them.';
    t = 0; cur = 0;

    const ev = [];
    const at = (time, fn) => ev.push({ time, fn });

    // 1 · diarization: the grey call track is scanned and splits into three voices
    at(0.4, () => scene.classList.add('e3-scanning'));
    at(0.8, () => pose('thinking', 'Three voices on their side. Splitting.'));
    at(2.3, () => {
      scene.classList.remove('e3-merged');
      setChip(laneChip('s1'), 's1', 'Speaker 1', 'pop');
      count.textContent = finalCount;
    });
    lines.filter((l) => l.spk !== 'me').forEach((l, k) => at(2.6 + k * 0.13, () => setChip(l.chip, l.first, LABEL[l.first], 'pop')));
    at(3.3, () => { slot.dataset.s = 'hint'; scene.classList.remove('e3-scanning'); pose('talking', 'Clean cut. One line is in the wrong lane.'); });

    // 2 · one line was given to the wrong voice: right-click, Change speaker → Speaker 3
    const wrong = lines.find((l) => l.first !== l.spk);
    at(3.7, () => { aim(wrong.chip, 0.3, 1.8); cursor.classList.add('on'); });
    at(3.8, () => aim(wrong.chip));
    at(4.5, () => { click(); wrong.li.classList.add('hl'); placeMenu(wrong.chip); menu.classList.add('on'); });
    at(4.9, () => aim(pick, 0.35));
    at(5.35, () => { pick.classList.add('on'); click(); });
    at(5.55, () => {
      menu.classList.remove('on');
      setChip(wrong.chip, wrong.spk, LABEL[wrong.spk], 'pop');
      scene.classList.remove('e3-wrong');
      pose('listening');
    });
    at(5.9, () => wrong.li.classList.remove('hl'));

    // 3 · rename Speaker 2 once: every line follows
    const first2 = lines.find((l) => l.spk === 's2');
    at(5.9, () => aim(first2.chip));
    at(6.55, () => { click(); slot.dataset.s = 'rename'; input.classList.add('focus'); pose('writing', 'Speaker 2. A name, then.'); });
    [...'Lena'].forEach((_, k) => at(6.95 + k * 0.16, () => { input.textContent = 'Lena'.slice(0, k + 1); }));
    at(7.7, () => aim(save));
    at(8.3, () => { click(); save.classList.add('press'); });
    const twos = [laneChip('s2'), ...lines.filter((l) => l.spk === 's2').map((l) => l.chip)];
    twos.forEach((chip, k) => at(8.45 + k * 0.13, () => setChip(chip, 's2', 'Lena', 'ripple')));
    at(8.5, () => { save.classList.remove('press'); input.classList.remove('focus'); pose('talking', 'Speaker 2 is Lena. Every line follows.'); });
    at(9.0, () => { slot.dataset.s = 'saved'; cursor.classList.remove('on'); });

    // 4 · her voice print is kept (never the audio)
    at(9.3, () => print.classList.add('in'));
    at(9.6, () => pose('writing', 'Her voice print is kept. Never the audio.'));

    // 5 · cut to the next meeting: a new line arrives and is already Lena
    at(11.0, () => { scene.classList.add('e3-cut'); next.classList.add('in', 'slam', 'rec'); pose('listening', 'Thursday. Someone speaks…'); });
    const words = finalNText.replace(/[.?!]$/, '').split(' ');
    at(11.5, () => nLine.classList.add('on', 'prov'));
    words.forEach((_, k) => at(11.5 + k * 0.16, () => { nText.textContent = `${words.slice(0, k + 1).join(' ')} …`; }));
    [1, 2, 3, 4].forEach((s) => at(11 + s * 0.42, () => { nTimer.textContent = `00:0${s}`; }));
    const firm = 11.5 + words.length * 0.16 + 0.35;
    at(firm, () => {
      nLine.classList.remove('prov');
      nText.textContent = finalNText;
      setChip(nChip, 's2', 'Lena', 'pop');
      sparkle();
      pose('talking', 'Lena. I know that voice.');
    });
    at(firm + 0.35, () => recog.classList.add('in'));
    at(firm + 0.9, () => scene.classList.remove('e3-cut'));
    at(firm + 2.2, () => { pose('idle', finalSay); next.classList.remove('rec'); });
    at(firm + 2.6, () => { replay.hidden = false; });
    events = ev.sort((x, y) => x.time - y.time);
  };

  const frame = (now) => {
    raf = 0;
    t += Math.min(0.25, (now - last) / 1000);
    last = now;
    while (cur < events.length && events[cur].time <= t) events[cur++].fn();
    if (visible && cur < events.length) raf = requestAnimationFrame(frame);
  };
  const play = () => {
    scene.classList.remove('e3-paused');
    if (!raf && cur < events.length) { last = performance.now(); raf = requestAnimationFrame(frame); }
  };
  const pause = () => { cancelAnimationFrame(raf); raf = 0; scene.classList.add('e3-paused'); };

  reset();
  ['listening', 'thinking', 'talking', 'writing', 'idle'].forEach((p) => preload(sprite('jin', p)));
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
    started = true;
    visible = scene.getBoundingClientRect().bottom > 0 && scene.getBoundingClientRect().top < innerHeight;
    if (!visible) scene.scrollIntoView({ behavior: 'smooth', block: 'center' });
    else play();
  });
})();
