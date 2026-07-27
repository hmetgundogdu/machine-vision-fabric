namespace Mvf.Cli.Tui;

/// <summary>
/// The TUI's glyph vocabulary — deliberately <b>ASCII-only</b> (every character is &lt; 0x80).
///
/// Why ASCII and not the prettier Unicode symbols (▶ ✔ ● → …): those live in the East-Asian
/// <i>Ambiguous</i>-width ranges. Spectre.Console measures them as one column and draws borders and
/// grid columns on that basis, but many terminals — notably Windows console hosts and the limited
/// fonts on panel-PC / industrial-PC targets — render them at <b>two</b> columns, or as a tofu box
/// (<c>?</c>) when the font lacks the glyph entirely. The mismatch pushes every following cell right,
/// so box content overruns its own border and the whole graph drifts a column at a time. Box-drawing
/// borders stay width-1 in the same terminals, which is why the borders look aligned while the text
/// inside them does not.
///
/// Keeping the whole set ASCII makes Spectre's width math and the terminal's rendered width agree on
/// every host, and colour already carries the semantics (green = done, red = fault, …) so the glyph
/// only has to be a distinct, single-column marker. All symbols are defined here once so the
/// vocabulary has a single source of truth; <c>GlyphsAsciiTests</c> guards that nobody reintroduces a
/// wide glyph.
/// </summary>
internal static class Glyphs
{
    // Node status icons — colour carries the meaning; the glyph is just a width-1 marker.
    public const string Idle    = "-";
    public const string Active  = ">";
    public const string Done    = "+";
    public const string Faulted = "x";

    // Kind-badge fallback for an unknown host kind.
    public const string UnknownKind = ".";

    // Edit affordances.
    public const string Editable = "*";   // "this field is live-editable"
    public const string Cursor   = ">";   // selection caret

    // Counts / separators.
    public const string Times     = "x";  // "xN cycles"
    public const string Separator = "|";  // control-bar item separator

    // Arrows.
    public const string ArrowRight  = ">";
    public const string ArrowLeft   = "<";
    public const string FlowRight   = "->";  // in→/out→ in the stats panel
    public const string MoveHint    = "<>";  // "←→ move" hint
    public const string FieldUpDown = "^v";  // "↑↓ field" hint

    // Edge shafts — data vs control get different ASCII so the mix stays legible without relying on colour.
    public const string DataShaft    = "-";
    public const string ControlShaft = "=";

    // Truncation marker — single column, so Truncate keeps reserving exactly one.
    public const string Ellipsis = "~";

    // Spinner frames for the active node — four width-1 ASCII glyphs cycled by Anim.SpinnerFrame.
    public const string SpinnerFrames = @"|/-\";

    // Logo pulse frames — a small width-1 ASCII mark that morphs beside the MVF logo while a run is live
    // (Anim.LogoFrame). Grows and shrinks so it reads as a heartbeat; all ASCII, so it never shifts a cell.
    public const string LogoPulseFrames = ".oOo";

    // Panel decorations.
    public const string Bullet   = "*";  // legend dots
    public const string NodeMark = "#";  // node-detail title
    public const string Invalid  = "x";  // "invalid pipeline" marker
    public const string None     = "-";  // "no value" / empty-port placeholder (was —)
}
