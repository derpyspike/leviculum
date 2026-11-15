package network.leviculum.loquax;

/**
 * The map frame's size and the pinch's floor (1796), the part of MapView
 * that is plain Java, so the host's tests run it (tests/fling.rs).
 *
 * The frame's size has one source: the margins Java puts into the
 * request. Java sizes the Bitmap by them before the call and the library
 * draws exactly them, so the Bitmap is always the frame. On 2026-11-13
 * the library decided the margins itself for a turning globe (1762) while
 * Java sized the Bitmap with its own, and every globe frame failed with
 * "map: the bitmap is 1836x3295, the frame 1224x2197".
 */
final class MapFrame {
    /**
     * The frame's margin past each edge, as a share of the screen: a
     * quarter drew a 1080 x 2400 frame moving at 64 ms on the host, half
     * at 111 ms (1131), and a smaller frame that lands sooner leaves less
     * of a pan to cover.
     */
    static final float OVERSCAN = 0.25f;

    private MapFrame() {
    }

    /**
     * The margin past each edge of a screen side `side` pixels long: none
     * while the last frame turned the globe, which is drawn in place and
     * never slid to show one (1762), a quarter of the side otherwise.
     */
    static int margin(int side, boolean turns) {
        return turns ? 0 : Math.round(side * OVERSCAN);
    }

    /**
     * Whether a Bitmap of `bw` x `bh` takes a frame of `w` x `h`: only of
     * that very size. After a rotation, or a frame that starts or stops
     * turning, a new one is made.
     */
    static boolean fits(int bw, int bh, int w, int h) {
        return bw == w && bh == h;
    }

    /**
     * One axis of the drag the fingers made since the last request, which
     * the frame answering that request did not absorb (order 1883): `off`
     * and `pinch` the fingers' offset and its pinch share now, `scale` the
     * scale now, the asked* values the same three when the request went.
     *
     * `off - pinch` is the drag share: a drag or a coast moves `off` alone,
     * a zoom of `by` (a pinch, a double tap, a fly step) scales both about
     * the same point and so scales their difference by `by`, as it scales
     * `scale`. The request carried the drag share it had then, which the
     * zooms since scaled by `scale / askedScale`; the rest came after it.
     *
     * A turning globe shows its frame at the pinch share alone, the drag
     * being the next frame's turn (1762), so a landing frame's pinch share
     * is its placed offset less this. Before 1883 it was the whole placed
     * offset, and every frame that landed while the finger moved on was
     * drawn shifted by the drag since its request, the earth sliding off
     * the picture's centre until the finger lifted (Lew, APK 254c3f3aa).
     * Without a drag since the request, the placed offset is untouched.
     */
    static float dragSince(float off, float pinch, float scale,
            float askedOff, float askedPinch, float askedScale) {
        return (off - pinch) - scale / askedScale * (askedOff - askedPinch);
    }

    /**
     * Whether a landing frame answers a pure turn of the globe (order
     * 1883): it turns, the request asked no zoom (`askedScale` 1), the
     * fingers zoomed none since (`scale` 1), and it is drawn at the zoom
     * of the frame before. Such a frame is drawn in place, whatever the
     * library says of the held ground: where the turn stops short of the
     * finger (north stays up, 1762), anchoring the held ground under the
     * finger slid the disk 3 px on a drag to the pole.
     */
    static boolean turnsOnly(boolean turns, float askedScale, float scale, long zoomBefore, long zoomAfter) {
        return turns && askedScale == 1f && scale == 1f && zoomBefore == zoomAfter;
    }

    /**
     * The factor a pinch of `by` may scale by, when the frame shown is at
     * `zoomMilli` and scaled by `scale` already: the view's zoom never
     * goes below `minZoomMilli`, the zoom where the world at its smallest
     * fills the screen, so a pinch out stops there and the world does not
     * shrink into blank space to bounce back. Zero or less for
     * `minZoomMilli` is no floor (a core before 1796).
     */
    static float zoomBy(float scale, float by, long zoomMilli, long minZoomMilli) {
        if (minZoomMilli <= 0 || scale <= 0) {
            return by;
        }
        double least = Math.pow(2, (minZoomMilli - zoomMilli) / 1000.0);
        if (scale * (double) by >= least) {
            return by;
        }
        return (float) (least / scale);
    }
}
