// Arena bounce, the uniblox sample game. Every scripted entity stays inside a 20 x 12 arena centred
// on the origin: at a wall, the script reflects the velocity component that points outward. The
// engine integrates position from velocity; the script only steers. It runs on whichever peer owns
// the entity, unchanged in Mode 1 (standalone), Mode 2 (peer-to-peer) and Mode 3 (server).
fn update(a) {
    if (a.x > 10.0 && a.vx > 0.0) || (a.x < -10.0 && a.vx < 0.0) {
        a.vx = -a.vx;
    }
    if (a.y > 6.0 && a.vy > 0.0) || (a.y < -6.0 && a.vy < 0.0) {
        a.vy = -a.vy;
    }
    a
}
