using System.Runtime.CompilerServices;

namespace Mvf.Engine.Tests;

/// <summary>
/// Guards the TUI glyph vocabulary against regressions: every glyph declared in <c>Glyphs.cs</c> must be
/// ASCII (&lt; 0x80). The "pretty" Unicode symbols (▶ ✔ ● → …) sit in East-Asian <i>Ambiguous</i>-width
/// ranges that Spectre.Console measures as one column but many terminals — Windows console hosts and the
/// limited fonts on panel-PC / industrial targets — render as two (or as a tofu box). That mismatch
/// drifts every node box past its own border. Colour, not the glyph, carries the status, so the whole
/// vocabulary stays ASCII; this test fails the moment someone reintroduces a wide glyph.
/// </summary>
public sealed class GlyphsAsciiTests
{
    [Fact]
    public void EveryGlyphConstantIsAscii()
    {
        var path = GlyphsSourcePath();
        Assert.True(File.Exists(path), $"Glyphs.cs not found at {path}");

        var offenders = new List<string>();
        var lineNo = 0;
        foreach (var line in File.ReadLines(path))
        {
            lineNo++;
            // Only the const declarations define the vocabulary, and only the quoted value counts — the
            // trailing comments deliberately name the wide glyphs each ASCII marker replaces.
            if (!line.Contains("public const string")) continue;
            var open  = line.IndexOf('"');
            var close = open >= 0 ? line.IndexOf('"', open + 1) : -1;
            if (open < 0 || close < 0) continue;
            foreach (var ch in line[(open + 1)..close])
                if (ch > 0x7F)
                    offenders.Add($"line {lineNo}: U+{(int)ch:X4} '{ch}'");
        }

        Assert.True(
            offenders.Count == 0,
            "Glyphs must stay ASCII (width-1 on every terminal). Non-ASCII found:\n" + string.Join("\n", offenders));
    }

    // tests/Mvf.Engine.Tests/GlyphsAsciiTests.cs  ->  src/cli/Mvf.Cli/Tui/Glyphs.cs
    private static string GlyphsSourcePath([CallerFilePath] string thisFile = "")
    {
        var testDir = Path.GetDirectoryName(thisFile)!;
        return Path.GetFullPath(Path.Combine(testDir, "..", "..", "src", "cli", "Mvf.Cli", "Tui", "Glyphs.cs"));
    }
}
