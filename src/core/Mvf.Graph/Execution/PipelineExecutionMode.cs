namespace Mvf.Graph.Execution;

/// <summary>
/// How the executor drives the graph. Pipelining is opt-in: the serial mode stays the default and the
/// reference for correctness, so a new scheduling model cannot quietly change the behaviour of an
/// existing pipeline.
/// </summary>
public enum PipelineExecutionMode
{
    /// <summary>
    /// One node at a time in topological order, a single frame in flight. Deterministic, and the cycle
    /// boundary is a quiesced point — which is what checkpoint/resume is built on.
    /// </summary>
    Serial = 0,

    /// <summary>
    /// Every node runs as its own stage, connected by bounded per-edge queues, so a slow worker no longer
    /// idles the source. A full queue blocks its producer, which is real block-the-producer backpressure
    /// rather than the serial mode's fail-fast approximation.
    /// </summary>
    Pipelined = 1
}
