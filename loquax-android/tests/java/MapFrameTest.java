package network.leviculum.loquax;

/**
 * The map frame's size and the pinch's floor (1796), on the host, run by
 * tests/fling.rs. The phone of 2026-11-13: a screen of 1224 x 2197, a
 * flat frame with its margins, then the globe, which turns and asks none;
 * the Bitmap of the flat frame does not take the globe's frame and a new
 * one is made, the turned-back flat frame again. A pinch out stops where
 * the world fills the screen, from any scale, and leaves a pinch in and a
 * core without the floor alone.
 */
final class MapFrameTest {
    private static int checks;

    public static void main(String[] args) {
        int w = 1224;
        int h = 2197;
        int flatW = w + 2 * MapFrame.margin(w, false);
        int flatH = h + 2 * MapFrame.margin(h, false);
        check(flatW == 1836 && flatH == 3295, "the flat frame " + flatW + "x" + flatH);
        int globeW = w + 2 * MapFrame.margin(w, true);
        int globeH = h + 2 * MapFrame.margin(h, true);
        check(globeW == w && globeH == h, "the turning globe's frame is the screen");
        check(!MapFrame.fits(flatW, flatH, globeW, globeH), "the flat Bitmap is not the globe's");
        check(MapFrame.fits(globeW, globeH, globeW, globeH), "the globe's Bitmap is reused");
        check(!MapFrame.fits(globeW, globeH, flatW, flatH), "turned back, a new one");
        // A rotation: the same margins rule, the sides swapped.
        int turnedW = h + 2 * MapFrame.margin(h, false);
        int turnedH = w + 2 * MapFrame.margin(w, false);
        check(!MapFrame.fits(flatW, flatH, turnedW, turnedH), "a rotation makes a new Bitmap");

        // The floor: the frame at z5, the world filling the screen at
        // z3.910. A pinch to a tenth stops at the floor's scale.
        long zoom = 5000;
        long floor = 3910;
        double least = Math.pow(2, (floor - zoom) / 1000.0);
        float scale = 1f;
        scale *= MapFrame.zoomBy(scale, 0.1f, zoom, floor);
        check(Math.abs(scale - least) < 1e-6, "stopped at " + scale + ", the floor " + least);
        float again = MapFrame.zoomBy(scale, 0.5f, zoom, floor);
        check(Math.abs(scale * again - least) < 1e-6, "a further pinch out changes nothing: " + again);
        check(MapFrame.zoomBy(scale, 2f, zoom, floor) == 2f, "a pinch in is the fingers'");
        check(MapFrame.zoomBy(1f, 0.9f, zoom, floor) == 0.9f, "above the floor, the fingers'");
        check(MapFrame.zoomBy(1f, 0.01f, zoom, 0) == 0.01f, "no floor from an older core");
        // At the floor already, a frame there: nothing smaller.
        float at = MapFrame.zoomBy(1f, 0.8f, floor, floor);
        check(Math.abs(at - 1f) < 1e-6, "at the floor a pinch out holds: " + at);

        dragOnTheGlobe();

        System.out.println(checks + " checks passed");
    }

    /*
     * MapView's transform, one axis, its statements as written there:
     * onScroll (off -= dx), zoomAbout (both about the focus), the request
     * (asked*), and place()'s turning branch for a frame that landed.
     */
    private static float off;
    private static float pinch;
    private static float scale = 1f;
    private static float askedOff;
    private static float askedPinch;
    private static float askedScale = 1f;

    private static void scroll(float dx) {
        off -= dx;
    }

    private static void zoomAbout(float by, float f) {
        scale *= by;
        off = by * (off - f) + f;
        pinch = by * (pinch - f) + f;
    }

    private static void request() {
        askedOff = off;
        askedPinch = pinch;
        askedScale = scale;
    }

    /**
     * A frame lands that drew the request at zoom ratio `a`; `placed` is
     * what place() computes for off (the held anchor or the asked
     * fallback, the library's part), `turns` the frame's word.
     */
    private static void land(boolean turns, double a, float placed, long z0, long z1) {
        float drag = MapFrame.dragSince(off, pinch, scale, askedOff, askedPinch, askedScale);
        float sigma = (float) (scale * a);
        if (MapFrame.turnsOnly(turns, askedScale, scale, z0, z1)) {
            placed = off - sigma * askedOff;
        }
        off = placed;
        pinch = off - drag;
        scale = sigma;
    }

    /** Where MapView draws the frame's left edge while the globe turns: shownX. */
    private static float shown() {
        return pinch;
    }

    /**
     * Order 1883, Lew on APK 254c3f3aa: a finger turning the globe moved
     * the earth off the picture's centre until it lifted. The finger moves
     * on while each frame renders; the frame that lands is drawn in place,
     * and stays there between landings. A frame whose held ground stops
     * short of the finger (a turn clamped at the pole) is no slide either.
     * A pinch keeps its own placement, the held anchor of order 1840.
     */
    private static void dragOnTheGlobe() {
        off = 0;
        pinch = 0;
        scale = 1f;
        request();
        long z = 1910;
        for (int e = 0; e < 40; e++) {
            scroll(-7f);
            check(shown() == 0f, "event " + e + ": the globe is drawn at " + shown() + " between frames");
            if (e % 2 == 0) {
                request();
            } else {
                // The frame asked one move ago lands: off - askedOff is
                // that move; the library's held ground stops 3 px short
                // of the finger every third frame (the clamped turn).
                float held = e % 3 == 0 ? 3f : 0f;
                land(true, 1.0, off - askedOff + held, z, z);
                check(Math.abs(shown()) < 1e-4f, "event " + e + ": a landed frame drawn at " + shown());
                check(Math.abs(off - 7f) < 1e-4f, "event " + e + ": the move since the request still " + off);
            }
        }
        // A pinch about a still focus: no drag since the request, the held
        // anchor's placement is the pinch share as before 1883.
        off = 0;
        pinch = 0;
        scale = 1f;
        request();
        zoomAbout(1.2f, 300f);
        request();
        zoomAbout(1.1f, 300f);
        float placed = 12.5f;
        land(true, 1 / 1.2, placed, 1910, 2173);
        check(Math.abs(shown() - placed) < 1e-4f, "a pinch's frame drawn at " + shown() + ", placed " + placed);
        check(Math.abs(MapFrame.dragSince(off, pinch, scale, askedOff, askedPinch, askedScale)) < 1e-4f,
                "no drag in a pinch");
        // A pinch whose focus moved 10 px after the request: that move is
        // the next turn, out of the pinch share, scaled by the zoom since.
        off = 0;
        pinch = 0;
        scale = 1f;
        request();
        zoomAbout(1.2f, 300f);
        request();
        scroll(-10f);
        zoomAbout(1.1f, 310f);
        land(true, 1 / 1.2, placed, 1910, 2173);
        check(Math.abs(shown() - (placed - 11f)) < 1e-4f, "the moved pinch drawn at " + shown());
        check(!MapFrame.turnsOnly(true, 1.2f, 1.2f, 1910, 2173), "a zoom is no pure turn");
        check(!MapFrame.turnsOnly(false, 1f, 1f, 1910, 1910), "a flat frame is no turn");
        check(!MapFrame.turnsOnly(true, 1f, 1f, 1910, 2000), "a frame at another zoom is no pure turn");
    }

    private static void check(boolean ok, String what) {
        if (!ok) {
            throw new AssertionError(what);
        }
        checks++;
    }
}
