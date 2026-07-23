namespace Mvf.Graph.Execution;

/// <summary>
/// Runtime options passed into the graph executor for a single execution run.
///
/// <para>A <b>record</b> on purpose: every layer that wants to add a callback (the execution host, the TUI
/// dashboard) must use <c>options with { ... }</c>. Both used to hand-copy a subset of the fields instead,
/// which silently dropped whatever had been added since — <c>--backpressure</c>, <c>--checkpoint-every</c>
/// and <c>--resume-dir</c> never reached the executor at all. Copying by hand is the bug; <c>with</c>
/// makes it unrepresentable.</para>
/// </summary>
public sealed record PipelineExecutionOptions
{
    /// <summary>
    /// Absolute path to the package directory.
    /// Used by integration modules to resolve relative asset paths.
    /// </summary>
    public required string PackageRoot { get; init; }

    /// <summary>
    /// Absolute path to the integration module plugin directory.
    /// </summary>
    public required string IntegrationsRoot { get; init; }

    /// <summary>
    /// Maximum number of source cycles to execute before stopping.
    /// 0 means run until the source is exhausted or cancelled.
    /// </summary>
    public int MaxCycles { get; init; } = 0;

    /// <summary>
    /// How often (in completed cycles) to checkpoint stateful, checkpointable node runners so a
    /// supervised worker always has a recent state to recover with. 0 disables periodic checkpoints.
    /// Checkpoints are taken at cycle boundaries, where the engine is quiesced (torn-free).
    /// </summary>
    public int CheckpointIntervalCycles { get; init; } = 0;

    /// <summary>
    /// Directory for durable checkpoints. When set, captured node states are persisted here each
    /// checkpoint and, on the next start, restored before the first cycle — so a run interrupted by an
    /// engine/process crash resumes where it left off (a clean, fully-consumed run clears them). Null
    /// keeps checkpoints in memory only (worker-crash recovery, but not engine-crash resume).
    /// </summary>
    public string? CheckpointDirectory { get; init; }

    /// <summary>
    /// What to do when an arena-backed producer cannot publish because the shared data plane is full.
    /// <see cref="BackpressurePolicy.Stall"/> (default) is lossless — it stops the run rather than lose a
    /// frame; <see cref="BackpressurePolicy.Drop"/> is lossy — it drops the frame for out-of-process
    /// consumers and keeps the source running. Inactive when the graph has no out-of-process workers.
    /// </summary>
    public BackpressurePolicy BackpressurePolicy { get; init; } = BackpressurePolicy.Stall;

    /// <summary>
    /// How the graph is driven. <see cref="PipelineExecutionMode.Serial"/> (default) runs one node at a
    /// time with a single frame in flight, so throughput is the sum of the stage latencies — deterministic
    /// and the mode every existing test assumes. <see cref="PipelineExecutionMode.Pipelined"/> runs each
    /// node as its own stage over bounded per-edge queues, so stages overlap and throughput approaches the
    /// slowest single stage.
    /// </summary>
    public PipelineExecutionMode ExecutionMode { get; init; } = PipelineExecutionMode.Serial;

    /// <summary>
    /// How many values one edge may hold in <see cref="PipelineExecutionMode.Pipelined"/> mode. This is
    /// the backpressure knob: a full queue blocks its producer. It also bounds arena occupancy, since
    /// every queued frame holds a slot.
    /// </summary>
    public int EdgeQueueCapacity { get; init; } = 2;

    /// <summary>
    /// Optional callback invoked at the end of each completed cycle.
    /// Use to observe real-time progress without polling.
    /// </summary>
    public Action<PipelineExecutionProgress>? OnCycleCompleted { get; init; }

    /// <summary>
    /// Optional callback invoked immediately after each node finishes executing.
    /// Fired before cycle-level routing — allows per-node observability.
    /// </summary>
    public Action<NodeExecutionEvent>? OnNodeExecuted { get; init; }
}
