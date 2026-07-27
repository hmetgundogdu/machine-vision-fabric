namespace Mvf.Cli.Tui;

/// <summary>
/// Frame-clock math for the dashboard's motion. Every value is derived from a wall-clock millisecond
/// reading (<see cref="System.Environment.TickCount64"/>), so the animation runs at a fixed real-time
/// rate no matter how often the screen actually repaints. Nothing here emits a glyph except by indexing
/// <see cref="Glyphs.SpinnerFrames"/>, and those are ASCII and width-1 — so animation never changes a
/// cell's width and can't reintroduce the drift the ASCII glyph pass removed (see the note in
/// <see cref="Glyphs"/>).
/// </summary>
internal static class Anim
{
    /// <summary>The spinner character for this instant — one width-1 ASCII frame from <see cref="Glyphs.SpinnerFrames"/>.</summary>
    public static string SpinnerFrame(long ms, int stepMs = 100)
    {
        var frames = Glyphs.SpinnerFrames;
        var i = (int)(Math.Max(0, ms) / Math.Max(1, stepMs) % frames.Length);
        return frames[i].ToString();
    }

    /// <summary>The logo pulse character for this instant — one width-1 ASCII frame from <see cref="Glyphs.LogoPulseFrames"/>.</summary>
    public static string LogoFrame(long ms, int stepMs = 180)
    {
        var frames = Glyphs.LogoPulseFrames;
        var i = (int)(Math.Max(0, ms) / Math.Max(1, stepMs) % frames.Length);
        return frames[i].ToString();
    }

    /// <summary>
    /// A triangle wave quantised to <paramref name="steps"/> buckets (0 .. steps-1 .. 0) over
    /// <paramref name="periodMs"/> — used to breathe a border between a few colour shades.
    /// </summary>
    public static int Triangle(long ms, int periodMs, int steps)
    {
        if (steps <= 1 || periodMs <= 0) return 0;
        var p   = (ms % periodMs) / (double)periodMs;     // 0 .. 1
        var tri = p < 0.5 ? p * 2 : (1 - p) * 2;          // 0 .. 1 .. 0
        var idx = (int)(tri * steps);
        return idx >= steps ? steps - 1 : idx;
    }

    /// <summary>
    /// The lit index (0 .. length-1) of a pulse travelling forward along a track of <paramref name="length"/>
    /// cells, completing one pass every <paramref name="periodMs"/>.
    /// </summary>
    public static int FlowPos(long ms, int length, int periodMs)
    {
        if (length <= 0 || periodMs <= 0) return 0;
        var i = (int)((ms % periodMs) / (double)periodMs * length);
        return i >= length ? length - 1 : i;
    }
}
