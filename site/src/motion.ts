// Sections arrive once, each in its own way, and the sphere marks on a ruler at the edge where the reader is.
const still = matchMedia("(prefers-reduced-motion: reduce)").matches;
const sections = [...document.querySelectorAll<HTMLElement>("main > section")];
document.documentElement.classList.add("moves");

/** Cut a heading into the lines it wraps to, so each rises from behind its own baseline (.ln in sections.css).
    The text is put back whole once they have landed, so it wraps freely again. */
function lines(el: HTMLElement) {
  const text = el.textContent ?? "";
  const words = text.split(/\s+/).filter(Boolean).map((word) => Object.assign(document.createElement("span"), { textContent: word }));
  el.replaceChildren(...words.flatMap((word, i) => (i ? [" ", word] : [word])));
  const rows: string[][] = [];
  let top = NaN;
  for (const word of words) {
    if (word.offsetTop !== top) rows.push([]);
    top = word.offsetTop;
    rows[rows.length - 1]!.push(word.textContent!);
  }
  el.replaceChildren(
    ...rows.flatMap((row, i) => {
      const line = Object.assign(document.createElement("span"), { className: "ln" });
      line.appendChild(Object.assign(document.createElement("span"), { textContent: row.join(" ") })).style.setProperty("--i", String(i));
      return i ? [" ", line] : [line];
    }),
  );
  setTimeout(() => (el.textContent = text), 1100 + rows.length * 90);
}

/** The right answers are counted up while their bars grow. */
function countUp(root: ParentNode, wait: number) {
  const cells = [...root.querySelectorAll<HTMLDataElement>("data")];
  for (const cell of cells) cell.textContent = "0";
  const start = performance.now() + wait;
  requestAnimationFrame(function step(t) {
    const k = Math.min(1, Math.max(0, (t - start) / 900));
    for (const cell of cells) cell.textContent = String(Math.round(Number(cell.value) * (1 - Math.pow(1 - k, 3))));
    if (k < 1) requestAnimationFrame(step);
  });
}

// what a section does, beyond its styles, the moment it arrives
const ARRIVALS: Record<string, (section: HTMLElement) => void> = {
  leaderboard: (section) => countUp(section, 300),
};

const arrive = new IntersectionObserver(
  (entries) => {
    for (const entry of entries) {
      if (!entry.isIntersecting) continue;
      const el = entry.target as HTMLElement;
      arrive.unobserve(el);
      if (!still) for (const heading of el.querySelectorAll<HTMLElement>("h2")) lines(heading);
      el.classList.add("seen");
      if (!still) ARRIVALS[el.id]?.(el);
    }
  },
  { rootMargin: "0px 0px -20% 0px" }, // once its top is a fifth of the way up the screen, where it will be read
);
for (const el of [...sections.slice(1), document.querySelector(".foot")!]) arrive.observe(el);

const rail = document.createElement("nav");
rail.className = "rail";
rail.setAttribute("aria-label", "Sections");
rail.innerHTML = sections.map((s) => `<a href="#${s.id}" aria-label="${s.dataset.name}" title="${s.dataset.name}"></a>`).join("");
const dot = document.createElement("i");
dot.className = "rail-dot";
for (const el of [rail, dot]) el.style.setProperty("--n", String(sections.length));
document.body.append(rail, dot);

// the section crossing the middle of the screen is where the reader is: the sphere goes to its mark, and the
// header underlines its link
const links = [...document.querySelectorAll<HTMLAnchorElement>(".nav nav a")];
const here = new IntersectionObserver(
  (entries) => {
    for (const entry of entries) {
      if (!entry.isIntersecting) continue;
      dot.style.setProperty("--at", String(sections.indexOf(entry.target as HTMLElement)));
      rail.classList.toggle("on-light", entry.target.matches(".band-paper, .band-tosca")); // ink ticks on the light grounds
      for (const a of links) a.classList.toggle("here", a.hash === `#${entry.target.id}`);
    }
  },
  { rootMargin: "-50% 0px -50% 0px" },
);
for (const s of sections) here.observe(s);
