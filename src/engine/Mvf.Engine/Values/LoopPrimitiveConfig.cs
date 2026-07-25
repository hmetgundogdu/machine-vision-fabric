using System.Text.Json.Nodes;
using Mvf.Graph.Values;

namespace Mvf.Engine.Values;

/// <summary>
/// The config of a <c>loop</c> node — the graph's iteration authority. It owns the termination policy and
/// carries the pause state; it is wired into the graph by a control edge from the pipeline's tail
/// (<c>sink.done → loop.done</c>), which is what makes it visible as the cycle's return point.
///
/// <para><see cref="Mode"/> is the termination policy (<see cref="LoopMode"/>). <see cref="Count"/> is the
/// cycle count, read only when <see cref="Mode"/> is <see cref="LoopMode.Count"/>.</para>
/// </summary>
public sealed record LoopPrimitiveConfig(
    string RawMode,
    LoopMode Mode,
    bool ModeKnown,
    int? Count)
{
    public static LoopPrimitiveConfig Read(JsonObject config)
    {
        ArgumentNullException.ThrowIfNull(config);

        var rawMode = ConfigJson.String(config, "mode") ?? "until-exhausted";
        var modeKnown = LoopModes.TryParse(rawMode, out var mode);

        int? count = config["count"] is JsonValue value && value.TryGetValue<int>(out var n) ? n : null;

        return new LoopPrimitiveConfig(
            RawMode: rawMode,
            Mode: mode,
            ModeKnown: modeKnown,
            Count: count);
    }
}
