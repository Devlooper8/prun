/* global document, window, IntersectionObserver */

const header = document.querySelector("[data-header]");
const toggle = document.querySelector(".nav-toggle");
const nav = document.querySelector(".site-nav");

function closeNav() {
  toggle?.setAttribute("aria-expanded", "false");
  nav?.classList.remove("is-open");
}

toggle?.addEventListener("click", () => {
  const open = toggle.getAttribute("aria-expanded") !== "true";
  toggle.setAttribute("aria-expanded", String(open));
  nav?.classList.toggle("is-open", open);
});

nav?.querySelectorAll("a").forEach((link) => link.addEventListener("click", closeNav));

window.addEventListener(
  "scroll",
  () => header?.classList.toggle("is-scrolled", window.scrollY > 20),
  { passive: true },
);

const animated = document.querySelectorAll("[data-animate]");
if ("IntersectionObserver" in window) {
  const observer = new IntersectionObserver(
    (entries) => {
      for (const entry of entries) {
        if (!entry.isIntersecting) continue;
        entry.target.classList.add("is-visible");
        observer.unobserve(entry.target);
      }
    },
    { threshold: 0.12, rootMargin: "0px 0px -40px" },
  );
  animated.forEach((element) => observer.observe(element));
} else {
  animated.forEach((element) => element.classList.add("is-visible"));
}
