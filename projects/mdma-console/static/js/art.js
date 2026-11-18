// Album art: a generated "white label" sleeve, with the real cover layered on top when there is one.
import { fnv, initials, MOSAIC } from "./logic.js";
import { h } from "./core.js";

/** CSS background for a name's placeholder sleeve (colours come from theme tokens). */
export function artBg(name) {
  const x = fnv(String(name).toLowerCase());
  const hue = 185 + (x % 70);
  const ang = [0, 45, 90, 135][(x >>> 7) % 4];
  const L = "var(--art-line)";
  const base = `linear-gradient(${(x >>> 11) % 360}deg, hsl(${hue} var(--art-s) var(--art-l)), hsl(${hue + 18} var(--art-s) var(--art-l2)))`;
  const cx = 20 + ((x >>> 3) % 60);
  const cy = 20 + ((x >>> 9) % 60);
  let pat;
  switch ((x >>> 5) % 4) {
    case 0:
      pat = `repeating-linear-gradient(${ang}deg, ${L} 0 1px, transparent 1px 7px)`;
      break;
    case 1:
      pat = `repeating-radial-gradient(circle at ${cx}% ${cy}%, ${L} 0 1px, transparent 1px 6px)`;
      break;
    case 2:
      pat = `linear-gradient(${L} 1px, transparent 1px) 0 0/14px 14px, linear-gradient(90deg, ${L} 1px, transparent 1px) 0 0/14px 14px`;
      break;
    default:
      pat = `linear-gradient(${ang}deg, transparent 0 58%, ${L} 58% 100%), linear-gradient(${ang + 90}deg, transparent 0 80%, ${L} 80% 100%)`;
  }
  return `${pat}, ${base}`;
}

/** URL of a track's cover. Only request it when the track has_cover. */
export function coverUrl(hash) {
  return `/cover/${encodeURIComponent(hash)}`;
}

/** Paint the placeholder onto an existing element. */
export function setArt(el, name) {
  el.style.backgroundImage = artBg(name);
  el.textContent = initials(name);
}

/**
 * Layer the real cover (track hash, or null for none) onto a sleeve element, replacing any cover
 * already there. If the image fails to load it removes itself and the placeholder shows.
 */
export function setCover(el, hash) {
  const old = el.querySelector("img");
  if (old) old.remove();
  if (!hash) return;
  el.appendChild(coverImg(hash, false));
}

/** A cover <img> that removes itself if it fails to load, so the placeholder shows. */
function coverImg(hash, lazy) {
  const img = h("img", { src: coverUrl(hash), alt: "", loading: lazy ? "lazy" : null });
  img.onerror = () => img.remove();
  return img;
}

/**
 * A sleeve element for `name`. With `opts.cover` (a track hash) a lazy <img> is layered on top;
 * with `opts.covers` (hashes of distinct albums) a 2x2 mosaic when there are MOSAIC of them, else
 * the first alone. An image that fails to load removes itself and the placeholder shows.
 */
export function art(name, opts = {}) {
  const covers = opts.covers || (opts.cover ? [opts.cover] : []);
  const mosaic = covers.length >= MOSAIC;
  const e = h("div", { class: "art" + (opts.round ? " round" : "") + (mosaic ? " mosaic" : ""), "aria-hidden": "true" });
  setArt(e, name);
  // A mosaic cell keeps its place (class q1..q4, see app.css) if another fails and removes itself.
  (mosaic ? covers.slice(0, MOSAIC) : covers.slice(0, 1)).forEach((hash, i) => {
    const img = coverImg(hash, true);
    if (mosaic) img.className = "q" + (i + 1);
    e.appendChild(img);
  });
  return e;
}
