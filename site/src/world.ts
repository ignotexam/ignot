// The scene's proportions, shared by the page (to place its text) and the scene (to draw).
export const WORLD = {
  MW: 1, // monolith width; 4 : 9 : 1 with its height and depth
  MH: 2.25,
  MD: 0.25,
  R: 0.5, // sphere radius: as wide as the monolith
  EYE: 0.45, // camera height above the floor
  DIST: 14, // camera distance
  TRACK_Y: 2.25 + 0.22 + 0.5, // the sphere's centre while it travels: floating just clear of the monolith's top
};

export interface View {
  w: number; // canvas size, CSS px
  h: number;
  u: number; // CSS px per world unit
  hy: number; // horizon, CSS px from the top
}
