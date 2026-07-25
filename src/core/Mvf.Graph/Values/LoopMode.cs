namespace Mvf.Graph.Values;

/// <summary>
/// How a <c>loop</c> decides it is done — the termination policy it owns as the graph's iteration
/// authority.
/// <list type="bullet">
///   <item><see cref="UntilExhausted"/> — run until the source stops producing frames (a folder played once).</item>
///   <item><see cref="Forever"/> — never stop on exhaustion; a finite source is rewound and replayed (a live
///     camera never exhausts anyway). This is what a source-level "loop/replay" flag used to do, moved to
///     the one place that owns iteration.</item>
///   <item><see cref="Count"/> — stop after a fixed number of cycles.</item>
/// </list>
/// </summary>
public enum LoopMode
{
    UntilExhausted,
    Forever,
    Count
}

public static class LoopModes
{
    public const string Supported = "until-exhausted, forever, count";

    public static bool TryParse(string? value, out LoopMode mode)
    {
        switch (value?.Trim().ToLowerInvariant())
        {
            case "until-exhausted": mode = LoopMode.UntilExhausted; return true;
            case "forever": mode = LoopMode.Forever; return true;
            case "count": mode = LoopMode.Count; return true;
            default: mode = LoopMode.UntilExhausted; return false;
        }
    }

    public static string ToToken(LoopMode mode) => mode switch
    {
        LoopMode.Forever => "forever",
        LoopMode.Count => "count",
        _ => "until-exhausted"
    };
}
