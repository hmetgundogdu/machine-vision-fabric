using Spectre.Console;
using Spectre.Console.Rendering;

namespace Mvf.Cli.Tui;

/// <summary>
/// How every full-screen TUI page gets onto the terminal. Extracted from the run dashboard when the
/// observer (<see cref="WatchDashboard"/>) needed the same behaviour: the painting strategy is a
/// portability decision, not a per-view detail, and two copies would drift.
/// </summary>
internal static class TuiPainter
{
    /// <summary>
    /// Draws a renderable at the top-left without a full clear (which flashes), then blanks the rest of the
    /// window so a previous, taller frame does not bleed through.
    /// </summary>
    public static void PaintInPlace(IRenderable renderable)
    {
        int cols, rows;
        try
        {
            cols = Console.WindowWidth  > 0 ? Console.WindowWidth  : 120;
            rows = Console.WindowHeight > 0 ? Console.WindowHeight : 40;
        }
        catch { cols = 120; rows = 40; }

        // Render to an ANSI string at the exact console width, then write it line by line, padding each
        // line out to the full width. Spectre draws every line only as wide as its content, so without the
        // padding a frame whose line is shorter than the previous frame's at that row leaves the old tail on
        // screen. Padding rather than an erase-escape keeps the Windows-console portability this in-place
        // strategy was chosen for, and the last row is held back so a full-width final line cannot wrap and
        // scroll the whole view.
        var lines = RenderToAnsi(renderable, cols).Split('\n');

        try
        {
            var r = 0;
            for (; r < lines.Length && r < rows - 1; r++)
            {
                var line = lines[r].TrimEnd('\r');
                Console.SetCursorPosition(0, r);
                Console.Write(line);
                var pad = cols - VisibleLength(line);
                if (pad > 0) Console.Write(new string(' ', pad));
            }

            // Blank any rows a previous, taller frame used.
            var blank = new string(' ', cols);
            for (; r < rows - 1; r++)
            {
                Console.SetCursorPosition(0, r);
                Console.Write(blank);
            }
        }
        catch { /* ignore on non-interactive hosts */ }
    }

    public static string RenderToAnsi(IRenderable renderable, int width)
    {
        var buffer  = new StringWriter();
        var console = AnsiConsole.Create(new AnsiConsoleSettings
        {
            Ansi        = AnsiSupport.Yes,
            ColorSystem = MapColorSystem(AnsiConsole.Profile.Capabilities.ColorSystem),
            Interactive = InteractionSupport.No,
            Out         = new AnsiConsoleOutput(buffer)
        });
        console.Profile.Width  = width;
        console.Profile.Height = 10_000;   // large, so nothing is clipped
        console.Write(renderable);
        return buffer.ToString();
    }

    /// <summary>Printable columns in a string, skipping ANSI escape sequences.</summary>
    public static int VisibleLength(string s)
    {
        var n = 0;
        for (var i = 0; i < s.Length; i++)
        {
            if (s[i] == '\x1b')
            {
                i++;
                if (i < s.Length && s[i] == '[')
                {
                    i++;
                    while (i < s.Length && !(s[i] >= '@' && s[i] <= '~')) i++;
                }
                continue;
            }
            n++;
        }
        return n;
    }

    private static ColorSystemSupport MapColorSystem(ColorSystem system) => system switch
    {
        ColorSystem.NoColors  => ColorSystemSupport.NoColors,
        ColorSystem.Legacy    => ColorSystemSupport.Legacy,
        ColorSystem.Standard  => ColorSystemSupport.Standard,
        ColorSystem.EightBit  => ColorSystemSupport.EightBit,
        ColorSystem.TrueColor => ColorSystemSupport.TrueColor,
        _                     => ColorSystemSupport.Standard
    };
}
