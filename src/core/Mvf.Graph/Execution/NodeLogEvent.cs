namespace Mvf.Graph.Execution;

/// <summary>
/// A log line a node emitted, on its way up to the operator. Delivered via
/// <see cref="PipelineExecutionOptions.OnNodeLog"/>.
///
/// <para>Sources: a co-located worker's structured <c>log</c> protocol message or a line it wrote to
/// stderr (see <c>protocol/README.md</c>), and an in-process module's <c>ModuleLog</c> calls. All three
/// surface here so a module author gets the same "log reaches the upper layer" contract regardless of
/// language or hosting.</para>
/// </summary>
public sealed class NodeLogEvent
{
    /// <summary>The node that emitted the line.</summary>
    public required string NodeId { get; init; }

    /// <summary>The module id behind the node (empty for an engine primitive).</summary>
    public required string ModuleId { get; init; }

    /// <summary>Severity: <c>debug</c>/<c>info</c>/<c>warn</c>/<c>error</c>, or <c>stderr</c> for a raw stderr line.</summary>
    public required string Level { get; init; }

    /// <summary>The log text.</summary>
    public required string Message { get; init; }

    /// <summary>When the engine observed the line.</summary>
    public DateTime TimestampUtc { get; init; } = DateTime.UtcNow;
}
