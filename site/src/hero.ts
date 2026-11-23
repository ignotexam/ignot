// The first screen: the week as a line in the sky, the sphere where the week is, the clock under the monolith.
import { $, DOT, WEEK, count, countdown, icon, local, now, roundAt, short, upcoming, utc } from "./data";
import { WORLD, type View } from "./world";

const exam = upcoming();
console.assert(exam.opens === Date.parse(exam.manifest.openAt), "the manifest's open time is not its round's time");

const root = document.documentElement;
const hero = $(".hero");
const track = $(".track");
const travelled = $(".travelled");
const grab = $(".grab");
const hint = $(".hint");
const clock = $(".clock");
const clockTime = $(".clock-time");
const canvas = $<HTMLCanvasElement>(".scene");
const still = matchMedia("(prefers-reduced-motion: reduce)").matches;

for (let days = 7; days >= 1; days--)
  track.insertAdjacentHTML("beforeend", `<i class="tick" style="left:${((7 - days) / 7) * 100}%;--i:${7 - days}"><span>${days}d</span></i>`);

// ---- the clock under the monolith
$(".clock-date").innerHTML =
  `<time datetime="${new Date(exam.opens).toISOString()}">${utc(exam.opens)}</time>` +
  `<span class="sep"> &middot; </span><span class="local">${local(exam.opens)} your time</span>`;

function tick() {
  const left = exam.opens - now();
  const open = left <= 0;
  const round = count.format(exam.manifest.openRound);
  $(".clock-name").textContent = open ? `${exam.manifest.title} is` : `${exam.manifest.title} opens in`;
  clockTime.textContent = open ? "open" : countdown(left);
  $(".clock-round").innerHTML =
    `drand round ${round}` + (open ? "" : `<span class="sep"> &middot; </span><span class="local">${count.format(exam.manifest.openRound - roundAt(now()))} to go</span>`);
  hero.classList.toggle("open", open);
}

// ---- the seal check: hash the sealed file here and compare with the manifest
const button = $<HTMLButtonElement>("#check");
const result = $(".check-result");
result.textContent = `${exam.manifest.id}${DOT}${exam.manifest.questionCount} questions${DOT}sha256 ${short(exam.manifest.ciphertextSha256)}`;
button.addEventListener("click", async () => {
  button.disabled = true;
  result.className = "check-result";
  result.textContent = "Hashing the sealed file in your browser...";
  try {
    const bytes = await (await fetch(exam.sealedUrl)).arrayBuffer();
    const hash = new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));
    const hex = "0x" + Array.from(hash, (b) => b.toString(16).padStart(2, "0")).join("");
    const same = hex === exam.manifest.ciphertextSha256;
    result.className = `check-result ${same ? "ok" : "bad"}`;
    result.innerHTML = same
      ? `${icon(true)}Seal intact. ${count.format(bytes.byteLength)} bytes hash to ${short(hex)}, the value in the manifest.`
      : `${icon(false)}The file hashes to ${short(hex)}, which is not the value in the manifest.`;
  } catch {
    result.className = "check-result bad";
    result.textContent = "The sealed file did not load. Reload the page and try again.";
  }
  button.disabled = false;
});

// ---- the sphere: it sits where the week is, and springs back when pulled
// three.js arrives after the words and the clock are on the page
const ready = { scene: false, type: false };
document.fonts.ready.then(() => (ready.type = true));
let scene: ReturnType<typeof import("./scene").createScene> | undefined;
import("./scene")
  .then(({ createScene }) => {
    scene = createScene(canvas, () => {
      ready.scene = true;
      canvas.classList.add("on");
    });
    scene.resize(view);
  })
  .catch(() => {
    // Without WebGL the two shapes are drawn in CSS instead: the monolith in the colours of its face, the sphere as
    // the logo's own picture on the element that already follows it (style.css, .flat).
    hero.classList.add("flat");
    ready.scene = true;
  });

let view: View & { x0: number; x1: number; ty: number; near: number; rides: boolean } = { w: 1, h: 1, u: 1, hy: 0, x0: 0, x1: 1, ty: 0, near: 1, rides: false };
function measure() {
  const style = getComputedStyle(hero);
  const read = (name: string) => parseFloat(style.getPropertyValue(name)) / 100;
  const w = hero.clientWidth;
  const h = hero.clientHeight;
  const u = (read("--mono-h") * h) / WORLD.MH;
  const hy = read("--horizon") * h;
  view = {
    w,
    h,
    u,
    hy,
    x0: read("--x0") * w,
    x1: read("--x1") * w,
    ty: hy - (WORLD.TRACK_Y - WORLD.EYE) * u,
    // how close the opening may stand to the mark and still keep the sphere on the screen
    near: Math.min(1.7, Math.max(1, (hy - 0.11 * h) / ((WORLD.TRACK_Y + WORLD.R - WORLD.EYE) * u))),
    rides: style.getPropertyValue("--clock-rides").trim() === "1",
  };
  const px = (n: number) => `${n.toFixed(2)}px`;
  for (const [name, value] of [["--u", u], ["--tx0", view.x0], ["--tx1", view.x1], ["--ty", view.ty], ["--my", hy + WORLD.EYE * u]] as const)
    hero.style.setProperty(name, px(value));
  scene?.resize(view);
  drawn.zoom = 1;
}

const pull = { x: 0, y: 0, vx: 0, vy: 0, held: false, fromX: 0, fromY: 0, quietAt: 0 };
const band = (d: number, max: number) => max * Math.tanh(d / max); // the further you pull, the less it gives
grab.addEventListener("pointerdown", (e) => {
  if (hero.classList.contains("open")) return;
  grab.setPointerCapture(e.pointerId);
  grab.classList.add("held");
  Object.assign(pull, { held: true, fromX: e.clientX - pull.x, fromY: e.clientY - pull.y, vx: 0, vy: 0 });
});
grab.addEventListener("pointermove", (e) => {
  if (!pull.held) return;
  pull.x = band(e.clientX - pull.fromX, view.u * 1.1);
  pull.y = band(e.clientY - pull.fromY, view.u * 0.5);
});
const release = () => {
  pull.held = false;
  grab.classList.remove("held");
};
grab.addEventListener("pointerup", release);
grab.addEventListener("pointercancel", release);

const look = { x: 0, y: 0, toX: 0, toY: 0 };
addEventListener("pointermove", (e) => {
  look.toX = (e.clientX / innerWidth) * 2 - 1;
  look.toY = (e.clientY / innerHeight) * 2 - 1;
});

// ---- the opening. The page starts on the mark: the monolith in the middle with the sphere over it. Then the
// monolith goes to its place and the sphere slides back along the week to where time is, the clock counting with
// it. `at` is the moment the lids start to part; the timings in style.css count from the same moment (html.go).
const born = performance.now();
const full = root.classList.contains("opening");
const lead = root.classList.contains("arrive") ? 1.05 : 0; // the short version joins where the lids would have finished
let at = full ? Infinity : lead ? born : -Infinity; // -Infinity: there is no opening, or it is over
// On a developer's machine the opening can be held at one moment (?open=1.4, in seconds) to look at a single
// frame of it. Never in a build.
const hold = import.meta.env.DEV ? parseFloat(new URLSearchParams(location.search).get("open") ?? "") : NaN;
const INPUTS = ["wheel", "touchstart", "keydown", "pointerdown", "scroll"] as const;
function finish(e?: Event) {
  if (e instanceof KeyboardEvent && ["Meta", "Control", "Alt", "Shift"].includes(e.key)) return;
  at = -Infinity;
  root.classList.remove("opening", "arrive", "go");
  clock.style.transform = "";
  tick(); // the clock may have been stopped mid-count
  for (const type of INPUTS) removeEventListener(type, finish);
}
if (at === -Infinity || scrollY > 4) finish();
else for (const type of INPUTS) addEventListener(type, finish, { passive: true });

const inOut = (x: number) => (x < 0.5 ? 4 * x * x * x : 1 - Math.pow(-2 * x + 2, 3) / 2);
const out = (x: number) => 1 - Math.pow(1 - x, 3);

// The scene only works while some of it is on screen.
let seen = true;
new IntersectionObserver(([entry]) => {
  seen = entry!.isIntersecting;
  if (!seen) finish();
}).observe(hero);

const drawn = { p: -1, said: "", half: 0, zoom: 1, rode: false, counted: "", mx: -1 }; // what the page last showed, so a frame only touches what changed
new ResizeObserver(measure).observe(hero);
measure();

let last = born;
let second = -1;
function frame(t: number) {
  requestAnimationFrame(frame);
  const dt = Math.min(0.05, (t - last) / 1000);
  last = t;
  if (Math.floor(now() / 1000) !== second) {
    second = Math.floor(now() / 1000);
    tick();
  }
  if (!seen) return;

  // the lids wait for the scene and the type, but not for long
  if (at === Infinity && ((ready.scene && ready.type && t - born > 450) || t - born > 2400)) {
    at = t;
    root.classList.add("go");
  }
  const k = isNaN(hold) || at === Infinity ? (t - at) / 1000 + lead : hold; // seconds into the opening
  if (!isNaN(hold))
    for (const a of document.getAnimations())
      if (a.timeline === document.timeline) {
        a.pause();
        a.currentTime = (hold - lead) * 1000;
      }
  if (k > 4.2 && at !== -Infinity) finish();
  const span = (from: number, to: number) => Math.min(1, Math.max(0, (k - from) / (to - from)));
  const back = inOut(span(1, 2.3)); // the sphere: from over the monolith to where the week is
  const pan = full ? back : 1; // the monolith: from the middle to its place, in the same breath, so the sphere never turns back
  const zoom = full ? 1 + (view.near - 1) * (1 - out(span(0, 1.9))) : 1; // the camera eases away from the mark

  const left = exam.opens - now();
  const shown = 1 - Math.min(1, Math.max(0, left / WEEK)) * back; // how far along the week the sphere is drawn
  const counted = back < 1 && left > 0 ? countdown(left * back, left >= 86_400_000) : ""; // the clock counts with the sphere, at its final width
  if (counted && counted !== drawn.counted) clockTime.textContent = drawn.counted = counted;

  if (!pull.held) {
    // a spring back to where time really is: a little overshoot, then rest
    pull.vx += (-220 * pull.x - 20 * pull.vx) * dt;
    pull.vy += (-220 * pull.y - 20 * pull.vy) * dt;
    pull.x += pull.vx * dt;
    pull.y += pull.vy * dt;
  }
  if (pull.held || Math.hypot(pull.x, pull.y) > 1) pull.quietAt = t + 1800;

  // laid out as the page is at rest, then moved with the monolith and scaled around the foot of the horizon
  const mx = view.w / 2 + (view.x1 - view.w / 2) * pan;
  if (!scene && mx !== drawn.mx) hero.style.setProperty("--mx", `${(drawn.mx = mx).toFixed(1)}px`); // the CSS monolith goes where the drawn one would
  const x = mx + (pull.x - (1 - shown) * (view.x1 - view.x0)) * zoom;
  const y = view.hy + (view.ty - view.hy + pull.y + (still ? 0 : Math.sin(t * 0.0011) * 0.035 * view.u)) * zoom;
  if (zoom !== drawn.zoom) scene?.lens(view.u * (drawn.zoom = zoom));

  const p = shown * out(span(1.9, 2.55)); // the copper line is drawn up to the sphere once the sphere is nearly home
  if (p !== drawn.p) travelled.style.transform = `scaleX(${(drawn.p = p).toFixed(4)})`;
  if (view.rides && pan < 1) clock.style.transform = `translateX(calc(-50% + ${(mx - view.x1).toFixed(1)}px))`; // under the monolith all the way
  else if (drawn.rode) clock.style.transform = "";
  drawn.rode = view.rides && pan < 1;

  grab.style.transform = `translate(${x}px, ${y}px)`;
  const said = hero.classList.contains("open") ? "" : t < pull.quietAt ? `Not before round ${count.format(exam.manifest.openRound)}.` : "try to pull it early";
  if (said !== drawn.said) {
    hint.textContent = drawn.said = said;
    drawn.half = hint.offsetWidth / 2 + 12; // measured only when the words change
  }
  hint.style.transform = `translate(${Math.min(view.w - drawn.half, Math.max(drawn.half, x))}px, ${y - view.u * 0.5 - 24}px) translateX(-50%)`;

  look.x += (look.toX - look.x) * 0.06;
  look.y += (look.toY - look.y) * 0.06;
  // the light on the copper drifts a little on its own, and follows the pointer
  scene?.draw({ x, y }, mx, { x: still ? look.x : look.x * 0.7 + Math.sin(t * 0.00035) * 0.3, y: look.y });
}
hero.classList.add("ready");
requestAnimationFrame(frame);
