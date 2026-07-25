using Mvf.Abstractions;
using Mvf.Graph.Execution;
using Mvf.Graph.Pipelines;

namespace Mvf.Engine.Execution;

/// <summary>
/// Turns an execution-time exception into an operator-readable message. The default
/// <c>ex.Message</c> alone drops the node that failed and any inner cause — for an out-of-process
/// module that is the module id and the child's own stderr (a Python traceback, a missing import),
/// which is exactly the "why". <see cref="Flatten"/> walks the inner-exception chain so that detail
/// reaches the report and the dashboard instead of a bare "Node activation failed:".
/// </summary>
internal static class ExecutionDiagnostics
{
    /// <summary>Message for a node whose activation (warmup / worker startup) threw.</summary>
    public static string ActivationFailure(PipelineNodeDefinition node, Exception ex) =>
        $"Node '{node.Id}'{ModulePart(node)} activation failed: {Flatten(ex)}";

    /// <summary>Message for a node that threw while executing a cycle.</summary>
    public static string ExecutionFailure(PipelineNodeDefinition node, Exception ex) =>
        $"Node '{node.Id}'{ModulePart(node)} threw during execution: {Flatten(ex)}";

    private static string ModulePart(PipelineNodeDefinition node) =>
        string.IsNullOrWhiteSpace(node.ModuleId) ? string.Empty : $" (module '{node.ModuleId}')";

    /// <summary>
    /// Builds the (level, message) sink for one node: tags each line with the node's id/module and forwards
    /// it to <paramref name="onNodeLog"/>. Built <b>once per node</b> and cached, then swapped into the
    /// ambient <see cref="ModuleLogContext.Holder"/> — so an in-process module's <c>ModuleLog</c> reaches
    /// the operator with no per-cycle allocation. The <see cref="NodeLogEvent"/> is created only when a
    /// module actually logs, not per cycle. Out-of-process workers ignore this and log over the protocol.
    /// </summary>
    public static Action<string, string> NodeLogSink(PipelineNodeDefinition node, Action<NodeLogEvent> onNodeLog)
    {
        var nodeId = node.Id;
        var moduleId = node.ModuleId ?? string.Empty;
        return (level, message) => onNodeLog(new NodeLogEvent
        {
            NodeId = nodeId,
            ModuleId = moduleId,
            Level = level,
            Message = message
        });
    }

    /// <summary>
    /// Joins an exception's message with its inner causes, de-duplicating identical adjacent messages
    /// (wrappers often repeat the inner text). Falls back to the type name when a message is empty.
    /// </summary>
    public static string Flatten(Exception ex)
    {
        var parts = new List<string>();
        for (Exception? e = ex; e is not null; e = e.InnerException)
        {
            var message = e.Message?.Trim();
            if (string.IsNullOrEmpty(message))
            {
                message = e.GetType().Name;
            }

            if (parts.Count == 0 || parts[^1] != message)
            {
                parts.Add(message);
            }
        }

        return string.Join(" → ", parts);
    }
}
