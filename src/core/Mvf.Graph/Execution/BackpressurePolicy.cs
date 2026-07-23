namespace Mvf.Graph.Execution;

/// <summary>
/// What the executor does when an arena-backed producer cannot publish its frame because the shared
/// data plane is momentarily <b>full</b> — every slot still carries a live buffer whose consumers have
/// not run yet. This is the lossless-vs-lossy choice the design calls for: a folder-sequence replay
/// wants every frame; a live camera wants bounded latency and would rather drop the newest frame than
/// stall the source.
///
/// <para>Note: a payload that can <i>never</i> fit a slot (larger than the slot capacity) is not
/// backpressure — it is a permanent sizing error and stops the run regardless of this policy, because
/// neither waiting nor dropping every frame forever is a useful outcome.</para>
/// </summary>
public enum BackpressurePolicy
{
    /// <summary>
    /// Lossless — never drop a frame. In the current <b>serial</b> executor there is no concurrent drain
    /// to wait for (a frame's consumers run later in the same cycle, after the producer), so an exhausted
    /// arena is a hard, actionable stop ("raise the slot count") rather than a silent loss. When a
    /// pipelined executor lands (M3+), Stall becomes a real block-the-producer wait until a slot frees.
    /// The safe default for an inspection platform: you opt <i>into</i> dropping.
    /// </summary>
    Stall,

    /// <summary>
    /// Lossy — drop the frame for its out-of-process (worker) consumers this cycle so latency stays
    /// bounded, count it (<see cref="PipelineExecutionReport.DroppedFrames"/>), and keep the source
    /// running. In-process consumers on the same output still receive the heap frame; only the arena hop
    /// is skipped. Right for a live camera where the newest frame matters more than every frame.
    /// </summary>
    Drop
}

/// <summary>Parsing for the per-node <c>backpressure</c> / manifest default string, so a source can
/// override the run-level policy (a folder-replay stalls, a live camera drops) as a real, validated field.</summary>
public static class BackpressurePolicies
{
    /// <summary>The accepted string values, for validation messages.</summary>
    public const string Supported = "stall, drop";

    /// <summary>Parses "stall" / "drop" (case-insensitive). Unknown → false.</summary>
    public static bool TryParse(string? value, out BackpressurePolicy policy)
    {
        switch (value?.Trim().ToLowerInvariant())
        {
            case "stall":
                policy = BackpressurePolicy.Stall;
                return true;
            case "drop":
                policy = BackpressurePolicy.Drop;
                return true;
            default:
                policy = BackpressurePolicy.Stall;
                return false;
        }
    }

    public static string ToWireString(BackpressurePolicy policy) =>
        policy == BackpressurePolicy.Drop ? "drop" : "stall";
}
