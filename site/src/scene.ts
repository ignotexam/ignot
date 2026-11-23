// The monolith and the copper sphere, drawn over the page's own sky. Everything stands at one depth, so the page
// can place its text with plain arithmetic: one world unit is `u` CSS pixels.
import * as THREE from "three";

import { WORLD, type View } from "./world";

const MONOLITH_VERT = /* glsl */ `
varying vec3 vObj; varying vec3 vWorld; varying vec3 vN;
void main() {
  vObj = position; vN = normal;
  vec4 w = modelMatrix * vec4(position, 1.0); vWorld = w.xyz;
  gl_Position = projectionMatrix * viewMatrix * w;
}`;

// Black glass, painted by hand in display colours: a teal-to-black face, and copper light that falls on it from
// wherever the sphere is. The mirrored copy fades with depth to make the floor look polished.
const MONOLITH_FRAG = /* glsl */ `
uniform vec3 uSphere; uniform float uMirror;
varying vec3 vObj; varying vec3 vWorld; varying vec3 vN;
const float MW = ${WORLD.MW.toFixed(2)}; const float MH = ${WORLD.MH.toFixed(2)}; const float R = ${WORLD.R.toFixed(2)}; const float EYE = ${WORLD.EYE.toFixed(2)};
vec3 face(float v) {
  vec3 c = mix(vec3(0.039, 0.353, 0.376), vec3(0.024, 0.290, 0.318), smoothstep(0.0, 0.23, v));
  c = mix(c, vec3(0.004, 0.227, 0.263), smoothstep(0.23, 0.36, v));
  c = mix(c, vec3(0.0, 0.161, 0.188), smoothstep(0.36, 0.50, v));
  c = mix(c, vec3(0.004, 0.090, 0.122), smoothstep(0.50, 0.67, v));
  c = mix(c, vec3(0.0, 0.039, 0.063), smoothstep(0.67, 0.85, v));
  return mix(c, vec3(0.0, 0.086, 0.094), smoothstep(0.85, 1.0, v));
}
void main() {
  float v = 0.5 - vObj.y / MH; // 0 at the top edge, 1 at the base
  float y = abs(vWorld.y);
  float front = step(0.5, vN.z);
  float lit = 1.0 / (1.0 + pow(distance(vec2(vWorld.x, MH), uSphere.xy) / 0.9, 2.0)); // 1 when the sphere is overhead
  vec3 c = face(v) * mix(1.4, 1.0, front) * mix(0.55, 1.0, lit); // darker glass until the sphere lights it

  // glass: the face shows the horizon back at eye height, the sky above it and the floor below, all faintly
  float up = (y - EYE) / (MH - EYE);
  vec3 seen = mix(vec3(0.26, 0.95, 0.88), up > 0.0 ? vec3(0.0, 0.37, 0.44) : vec3(0.06, 0.74, 0.70), smoothstep(0.0, 0.55, abs(up)));
  c += seen * 0.115 * front;

  float d = max(distance(vec3(vWorld.x, y, vWorld.z), uSphere) - R, 0.0); // to the sphere's surface
  float near = 1.0 / (1.0 + pow(d / 0.30, 3.0));
  vec3 warm = mix(vec3(0.72, 0.32, 0.14), vec3(1.0, 0.72, 0.38), near);
  c = mix(c, warm, smoothstep(0.0, 0.45, near));

  vec3 edge = mix(vec3(0.17, 0.50, 0.53), vec3(1.0, 0.955, 0.80), lit); // sky on the top edge, copper light when the sphere is near
  c = mix(c, edge, smoothstep(0.011, 0.003, v) * front);

  float side = abs(vObj.x) / (MW * 0.5);
  c = mix(c, vec3(0.30, 0.60, 0.62), smoothstep(0.972, 1.0, side) * front * 0.5);

  float a = uMirror > 0.0 ? 1.0 : 0.66 * (1.0 - smoothstep(0.0, 0.5, y));
  gl_FragColor = vec4(c * a, a);
}`;

// What the copper reflects is the logo's own sphere, unwrapped into an environment (public/env.jpg), so the live
// sphere carries the same light. The copper is already in that picture; the metal itself is left white.
function reflect(renderer: THREE.WebGLRenderer, scene: THREE.Scene, done: (lit: boolean) => void) {
  new THREE.TextureLoader().load(
    "/env.jpg",
    (tex) => {
      tex.mapping = THREE.EquirectangularReflectionMapping;
      tex.colorSpace = THREE.SRGBColorSpace;
      const pmrem = new THREE.PMREMGenerator(renderer);
      scene.environment = pmrem.fromEquirectangular(tex).texture;
      tex.dispose();
      pmrem.dispose();
      done(true);
    },
    undefined,
    () => done(false), // the picture did not load: the monolith still stands, without its sphere
  );
}

/** `ready` is called once the reflection is built and the shaders are compiled: the first frame will not stall. */
export function createScene(canvas: HTMLCanvasElement, ready: () => void) {
  const renderer = new THREE.WebGLRenderer({ canvas, antialias: true, alpha: true });
  renderer.setClearColor(0x000000, 0);
  renderer.setPixelRatio(Math.min(window.devicePixelRatio, 2));

  const scene = new THREE.Scene();
  const camera = new THREE.PerspectiveCamera(30, 1, 1, 60);
  camera.position.set(0, WORLD.EYE, WORLD.DIST);

  const sphere = new THREE.Mesh(
    new THREE.SphereGeometry(WORLD.R, 96, 64),
    new THREE.MeshStandardMaterial({ color: 0xffffff, metalness: 1, roughness: 0.05 }),
  );
  sphere.visible = false; // a mirror with nothing to reflect is a black ball
  scene.add(sphere);

  const uSphere = { value: sphere.position };
  const glass = (mirror: number) =>
    new THREE.ShaderMaterial({
      vertexShader: MONOLITH_VERT,
      fragmentShader: MONOLITH_FRAG,
      uniforms: { uSphere, uMirror: { value: mirror } },
      side: THREE.DoubleSide,
      transparent: mirror < 0,
      depthWrite: mirror > 0,
      premultipliedAlpha: true,
    });
  const box = new THREE.BoxGeometry(WORLD.MW, WORLD.MH, WORLD.MD);
  const monolith = new THREE.Mesh(box, glass(1));
  const reflection = new THREE.Mesh(box, glass(-1));
  monolith.position.y = WORLD.MH / 2;
  reflection.position.y = -WORLD.MH / 2;
  reflection.scale.y = -1;
  scene.add(monolith, reflection);

  reflect(renderer, scene, (lit) => {
    sphere.visible = lit;
    renderer.compileAsync(scene, camera).then(ready, ready);
  });

  let view: View = { w: 1, h: 1, u: 1, hy: 0 };
  const toWorldX = (px: number) => (px - view.w / 2) / view.u;
  const toWorldY = (px: number) => WORLD.EYE + (view.hy - px) / view.u;

  // One world unit is `u` CSS px. Changing only `u` moves the camera in or out around the horizon line.
  function lens(u: number) {
    view.u = u;
    camera.fov = THREE.MathUtils.radToDeg(2 * Math.atan(view.h / u / (2 * WORLD.DIST)));
    camera.updateProjectionMatrix();
    // Shift the lens instead of tilting the camera: the horizon lands where the page drew it and verticals stay vertical.
    camera.projectionMatrix.elements[9] = 2 * (view.hy / view.h - 0.5);
    camera.projectionMatrixInverse.copy(camera.projectionMatrix).invert();
  }

  return {
    resize(next: View) {
      view = { ...next };
      renderer.setSize(next.w, next.h, false);
      camera.aspect = next.w / next.h;
      lens(next.u);
    },
    lens,
    // Positions arrive in CSS px; `look` is the pointer in -1..1 and only moves the highlights.
    draw(spherePx: { x: number; y: number }, monolithPx: number, look: { x: number; y: number }) {
      sphere.position.set(toWorldX(spherePx.x), toWorldY(spherePx.y), 0);
      monolith.position.x = reflection.position.x = toWorldX(monolithPx);
      scene.environmentRotation.set(look.y * 0.12, look.x * 0.45, 0);
      renderer.render(scene, camera);
    },
  };
}
