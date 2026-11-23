// The docs page: the shared header, and the list of contents marks the part being read.
import "./nav";

const links = [...document.querySelectorAll<HTMLAnchorElement>(".toc a")];
const here = new IntersectionObserver(
  (entries) => {
    for (const entry of entries) {
      if (entry.isIntersecting) for (const a of links) a.classList.toggle("here", a.hash === `#${entry.target.id}`);
    }
  },
  { rootMargin: "0px 0px -70% 0px" }, // a heading counts once it is in the top third of the screen
);
for (const a of links) here.observe(document.querySelector(a.hash)!);
