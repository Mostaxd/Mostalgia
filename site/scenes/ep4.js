// Episode 4 · summaries: what happens once the summary exists.
// Kaito checks the AI connections and picks one for summaries and one for Ask,
// the recap brews, gets copied into the team chat and exported, and the
// decision log fills a timeline where a later decision supersedes an earlier one.
// The HTML holds the finished scene (what you see without JS or with reduced
// motion); here it is rewound and replayed on a clock that only runs on screen.
(() => {
  const S = window.MostalgiaSite;
  const scene = document.getElementById('e4Scene');
  if (!S || !scene) return;
  const { reduce, $, $$, sprite, type, preload } = S;

  // the supersede arc is drawn in real pixels so its stroke stays even at any width
  const arc = $('.e4-arc', scene);
  const drawArc = () => {
    const w = arc.clientWidth, h = arc.clientHeight;
    if (!w || !h) return;
    const x1 = w * 0.875, x0 = w * 0.125;
    arc.setAttribute('viewBox', `0 0 ${w} ${h}`);
    $('.e4-line', arc).setAttribute('d', `M${x1} ${h + 6}C${x1} 0 ${x0} 0 ${x0} ${h + 2}`);
    $('.e4-head', arc).setAttribute('d', `M${x0 - 6} ${h - 6}L${x0} ${h + 2}L${x0 + 6} ${h - 6}`);
  };
  drawArc();
  new ResizeObserver(drawArc).observe(arc);

  if (reduce) return;

  const box = $('.e4-stagebox', scene);
  const say = $('#e4Say'), kaito = $('#e4Kaito'), replay = $('#e4Replay');
  const finalSay = say.textContent;
  const conns = $$('.e4-conn', scene).map((li) => {
    const em = $('.e4-st em', li);
    return { li, em, final: em.textContent, name: $('.e4-name', li).textContent, state: li.dataset.state };
  });
  const picks = $$('.e4-pick', scene).map((p) => ({ p, name: $('.e4-pick-v b', p), dot: $('.e4-pick-v .e4-dot', p), think: $('.e4-think', p) }));
  const brew = $('.e4-brew', scene), brewTxt = $('#e4Brew');
  const card = $('#e4Card'), lines = $$('.e4-r', card);
  const copy = $('#e4Copy'), copyTxt = $('.e4-copy-t', copy), cursor = $('.e4-cursor', copy);
  const mine = $('#e4Mine'), paste = $('#e4Paste'), react = $('.e4-react', mine);
  const exports = $$('.e4-exports li', scene);
  const decs = $$('.e4-dec', scene);
  const copyLabel = copyTxt.textContent;

  const pose = (p, line) => {
    const src = sprite('kaito', p);
    if (kaito.getAttribute('src') !== src) { kaito.src = src; kaito.classList.remove('hop'); void kaito.offsetWidth; kaito.classList.add('hop'); }
    if (line) type(say, line);
  };
  const retrigger = (el, cls) => { el.classList.remove(cls); void el.offsetWidth; el.classList.add(cls); };

  // a picker flips to connection i: it lands if that one is connected, shakes if not
  const pick = (k, i) => {
    const { p, name, dot, think } = picks[k], c = conns[i];
    conns.forEach((o, j) => o.li.classList.toggle('hl', j === i));
    name.textContent = c.name;
    dot.className = `e4-dot ${c.state}`;
    p.classList.remove('nope', 'yes');
    retrigger(p, 'flip');
    if (c.state === 'ok') { p.classList.add('yes'); c.li.classList.add('picked'); if (think) think.style.opacity = 1; }
    else { void p.offsetWidth; p.classList.add('nope'); }
  };

  // Web Animations (the flying recap) pause with the clock
  const anims = new Set();
  const fly = () => {
    const o = box.getBoundingClientRect(), a = card.getBoundingClientRect(), b = paste.getBoundingClientRect();
    const g = card.cloneNode(true);
    g.removeAttribute('id');
    g.classList.add('e4-ghost');
    Object.assign(g.style, { left: `${a.left - o.left}px`, top: `${a.top - o.top}px`, width: `${a.width}px` });
    box.append(g);
    const dx = b.left - a.left, dy = b.top - a.top, s = b.width / a.width;
    const an = g.animate([
      { transform: 'none' },
      { transform: `translate(${dx * 0.5}px, ${dy * 0.5 - 50}px) rotate(-5deg) scale(${(1 + s) / 2})`, offset: 0.5 },
      { transform: `translate(${dx}px, ${dy}px) scale(${s})`, opacity: 0.6 },
    ], { duration: 800, easing: 'cubic-bezier(.45,0,.3,1)', fill: 'forwards' });
    anims.add(an);
    an.onfinish = () => { anims.delete(an); g.remove(); };
  };

  let events = [], next = 0, t = 0, last = 0, raf = 0;
  let started = false, visible = false;

  const reset = () => {
    anims.forEach((an) => an.cancel());
    anims.clear();
    $$('.e4-ghost', box).forEach((g) => g.remove());
    scene.classList.add('e4-anim');
    conns.forEach((c) => { c.li.classList.remove('chk', 'done', 'hl', 'picked'); c.em.textContent = 'Not checked'; });
    picks.forEach(({ p, name, dot, think }) => {
      p.classList.remove('flip', 'nope', 'yes');
      name.textContent = 'Choose…';
      dot.className = 'e4-dot';
      if (think) think.style.opacity = 0;
    });
    brew.classList.remove('on', 'full');
    brewTxt.textContent = 'Waiting for an AI…';
    [...lines, cursor, mine, react, ...exports, ...decs, arc].forEach((el) => el.classList.remove('in', 'sup', 'tip'));
    copy.classList.remove('press');
    copyTxt.textContent = copyLabel;
    kaito.src = sprite('kaito', 'idle');
    type(say, 'Your kitchen, your beans. Who\'s awake today?');
    t = 0; next = 0;

    const ev = [];
    const at = (time, fn) => ev.push({ time, fn });

    // 1 · bring your own AI: each connection answers, then the pickers flip
    at(0.5, () => pose('thinking'));
    conns.forEach((c, i) => {
      at(0.6 + i * 0.35, () => { c.li.classList.add('chk'); c.em.textContent = 'Checking…'; });
      at(1.2 + i * 0.35, () => { c.li.classList.replace('chk', 'done'); c.em.textContent = c.final; });
    });
    at(2.5, () => { pick(0, 3); pose('error', 'That endpoint is failing: connection refused.'); });
    at(3.4, () => { pick(0, 2); pose('thinking', 'Provider B isn\'t set up yet…'); });
    at(4.3, () => { pick(0, 0); pose('talking', 'Local model writes the summaries. Cloud A takes Ask.'); });
    at(5.0, () => pick(1, 1));
    at(5.6, () => conns.forEach((c) => c.li.classList.remove('hl')));

    // 2 · the recap brews, gets copied into the team chat, then exports stamp out
    at(5.8, () => { brew.classList.add('on'); brewTxt.textContent = 'Brewing recap…'; pose('writing', 'Let it steep a moment…'); });
    at(5.9, () => brew.classList.add('full'));
    lines.forEach((l, i) => at(6.7 + i * 0.22, () => l.classList.add('in')));
    at(7.5, () => { brewTxt.textContent = 'Recap ready'; });
    at(7.7, () => cursor.classList.add('in'));
    at(8.3, () => copy.classList.add('press'));
    at(8.45, () => { copy.classList.remove('press'); copyTxt.textContent = 'Copied ✓'; pose('talking', 'One click, and it pours straight into the team chat.'); fly(); });
    at(8.9, () => cursor.classList.remove('in'));
    at(9.25, () => mine.classList.add('in'));
    at(9.8, () => react.classList.add('in'));
    exports.forEach((x, i) => at(10.0 + i * 0.2, () => x.classList.add('in')));
    at(10.2, () => { copyTxt.textContent = copyLabel; });

    // 3 · the decision log, week by week; the last decision supersedes the first
    at(10.9, () => pose('writing', 'And every decision goes in my ledger.'));
    decs.forEach((d, i) => at(11.3 + i * 0.55, () => d.classList.add('in')));
    at(13.4, () => { arc.classList.add('in'); pose('thinking'); });
    at(14.3, () => {
      arc.classList.add('tip');
      decs.forEach((d) => (d.classList.contains('old') || d.classList.contains('new')) && d.classList.add('sup'));
      pose('talking', 'The 21st beats the 14th. The old one stays, marked superseded.');
    });
    at(16.6, () => pose('idle', finalSay));
    at(17.2, () => { replay.hidden = false; });
    events = ev.sort((x, y) => x.time - y.time);
  };

  const frame = (now) => {
    raf = 0;
    t += Math.min(0.1, (now - last) / 1000);
    last = now;
    while (next < events.length && events[next].time <= t) events[next++].fn();
    if (visible && next < events.length) raf = requestAnimationFrame(frame);
  };
  const play = () => {
    scene.classList.remove('e4-paused');
    anims.forEach((an) => an.play());
    if (!raf && next < events.length) { last = performance.now(); raf = requestAnimationFrame(frame); }
  };
  const pause = () => {
    cancelAnimationFrame(raf);
    raf = 0;
    scene.classList.add('e4-paused');
    anims.forEach((an) => an.pause());
  };

  reset();
  ['thinking', 'error', 'talking', 'writing'].forEach((p) => preload(sprite('kaito', p)));
  // start once ~40% of the scene (or of the screen, on tall phones) is in view; pause whenever it leaves
  new IntersectionObserver(([e]) => {
    const vh = e.rootBounds ? e.rootBounds.height : innerHeight;
    const seen = e.intersectionRect.height / Math.min(e.boundingClientRect.height, vh);
    visible = started ? e.isIntersecting : seen >= 0.4;
    if (visible) { started = true; play(); } else pause();
  }, { threshold: [0, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 1] }).observe(box);

  replay.addEventListener('click', () => { pause(); reset(); started = visible = true; play(); });
})();
