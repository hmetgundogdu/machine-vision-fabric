using System.Text.Json;
using System.Text.Json.Nodes;
using Mvf.Engine.Modules;
using Mvf.Graph.Integrations;
using Mvf.Graph.Pipelines;

namespace Mvf.Engine.Pipelines;

/// <summary>
/// Expands a <b>lean</b> pipeline definition into the <b>rich</b>
/// <see cref="PipelineDefinition"/> the validator and executor already consume — so those
/// stay unchanged. Authors write only what is not derivable:
/// <list type="bullet">
///   <item>module node: <c>{ "id": "blackCheck1", "module": "mvf.black-screen-check", "config": {…} }</c></item>
///   <item>primitive node: <c>{ "id": "fork1", "primitive": "fork" }</c> (fork/switch outputs come from its leaving edges)</item>
///   <item>edge: <c>{ "from": "camera1.frame", "to": "fork1.frame" }</c> (id + kind derived)</item>
/// </list>
/// Ports and category come from the module's <see cref="IntegrationCapabilityKind"/> (read
/// from a lightweight <see cref="ModuleManifest"/> catalog — no DLL load) or from the
/// primitive type. Nodes/edges already in the rich shape pass through unchanged, so a mixed
/// or fully-rich file still works.
/// </summary>
public sealed class PipelineExpander
{
    private static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web);

    private const string DataFrameType = "data/frame";
    private const string ClassificationSignalType = "control/classification";
    private const string BooleanGateSignalType = "control/boolean-gate";

    public PipelineDefinition Expand(string pipelineJson, IReadOnlyDictionary<string, ModuleCatalogEntry> catalog)
    {
        ArgumentNullException.ThrowIfNull(pipelineJson);
        ArgumentNullException.ThrowIfNull(catalog);

        JsonObject root;
        try
        {
            root = JsonNode.Parse(pipelineJson) as JsonObject
                   ?? throw new PipelineExpansionException("Pipeline JSON must be a JSON object.");
        }
        catch (JsonException ex)
        {
            throw new PipelineExpansionException($"Pipeline JSON is invalid: {ex.Message}");
        }

        var nodeArray = root["nodes"] as JsonArray
            ?? throw new PipelineExpansionException("Pipeline is missing a 'nodes' array.");
        var edgeArray = root["edges"] as JsonArray ?? [];

        // Parse edges first: fork/switch output ports are derived from the ports leaving a node.
        var parsedEdges = edgeArray.Select((edge, index) => ParseEdge(edge, index)).ToList();
        var leavingPortsByNode = parsedEdges
            .GroupBy(e => e.FromNode, StringComparer.OrdinalIgnoreCase)
            .ToDictionary(
                g => g.Key,
                g => (IReadOnlyList<string>)g.Select(e => e.FromPort).Distinct(StringComparer.OrdinalIgnoreCase).ToList(),
                StringComparer.OrdinalIgnoreCase);

        var nodes = new List<PipelineNodeDefinition>(nodeArray.Count);
        foreach (var nodeNode in nodeArray)
        {
            if (nodeNode is not JsonObject nodeObj)
            {
                throw new PipelineExpansionException("Each entry in 'nodes' must be a JSON object.");
            }

            nodes.Add(ExpandNode(nodeObj, catalog, leavingPortsByNode));
        }

        // Now that every node's output ports are known, an edge's kind (data|control) is the
        // channel of its source output port; unresolved ports default to data — the validator
        // will flag the real problem (missing node/port) with a precise message.
        var outputChannelByPort = new Dictionary<(string Node, string Port), string>();
        foreach (var node in nodes)
        {
            foreach (var output in node.Outputs)
            {
                outputChannelByPort[(node.Id, output.Name)] = output.Channel;
            }
        }

        var edges = new List<PipelineEdgeDefinition>(parsedEdges.Count);
        foreach (var parsed in parsedEdges)
        {
            var kind = parsed.ExplicitKind
                ?? (outputChannelByPort.TryGetValue((parsed.FromNode, parsed.FromPort), out var channel) ? channel : "data");

            edges.Add(new PipelineEdgeDefinition
            {
                Id = parsed.Id ?? $"e{parsed.Index + 1}",
                Kind = kind,
                From = new PipelinePortReference { NodeId = parsed.FromNode, Port = parsed.FromPort },
                To = new PipelinePortReference { NodeId = parsed.ToNode, Port = parsed.ToPort },
                Condition = parsed.Condition
            });
        }

        return new PipelineDefinition
        {
            Name = GetString(root, "name") ?? "pipeline",
            Version = GetString(root, "version") ?? "0.1.0",
            Description = GetString(root, "description") ?? "Typed pipeline graph definition.",
            RuntimeMode = GetString(root, "runtimeMode") ?? "headless-dataset-collection",
            Capabilities = root["capabilities"] is JsonArray caps
                ? caps.Select(c => c?.GetValue<string>() ?? string.Empty).Where(s => s.Length > 0).ToList()
                : [],
            Nodes = nodes,
            Edges = edges
        };
    }

    private static PipelineNodeDefinition ExpandNode(
        JsonObject nodeObj,
        IReadOnlyDictionary<string, ModuleCatalogEntry> catalog,
        IReadOnlyDictionary<string, IReadOnlyList<string>> leavingPortsByNode)
    {
        // Already rich? Pass it straight through the serializer.
        if (nodeObj["kind"] is not null && nodeObj["module"] is null && nodeObj["primitive"] is null)
        {
            return nodeObj.Deserialize<PipelineNodeDefinition>(JsonOptions)
                   ?? throw new PipelineExpansionException("A rich node could not be parsed.");
        }

        var id = GetString(nodeObj, "id")
            ?? throw new PipelineExpansionException("A node is missing its required 'id'.");

        if (nodeObj["module"] is not null)
        {
            return ExpandModuleNode(id, nodeObj, catalog);
        }

        if (nodeObj["primitive"] is not null)
        {
            return ExpandPrimitiveNode(id, nodeObj, leavingPortsByNode);
        }

        throw new PipelineExpansionException(
            $"Node '{id}' must declare one of: 'module', 'primitive', or a rich 'kind'.");
    }

    private static PipelineNodeDefinition ExpandModuleNode(
        string id,
        JsonObject nodeObj,
        IReadOnlyDictionary<string, ModuleCatalogEntry> catalog)
    {
        var moduleId = GetString(nodeObj, "module")!;
        if (!catalog.TryGetValue(moduleId, out var entry))
        {
            throw new PipelineExpansionException(
                $"Node '{id}' references unknown module '{moduleId}'. " +
                "Ensure a module.json with that id exists under the integrations root.");
        }

        var (category, inputs, outputs) = StandardModulePorts(id, entry.Manifest.Kind);

        return new PipelineNodeDefinition
        {
            Id = id,
            DisplayName = GetString(nodeObj, "displayName") ?? entry.Manifest.Name,
            Kind = "integration-module",
            Category = category,
            ModuleId = moduleId,
            ActivationMode = GetString(nodeObj, "activationMode"),
            Backpressure = GetString(nodeObj, "backpressure"),
            Parallelism = GetInt(nodeObj, "parallelism"),
            Config = CloneConfig(nodeObj),
            Inputs = inputs,
            Outputs = outputs
        };
    }

    private static PipelineNodeDefinition ExpandPrimitiveNode(
        string id,
        JsonObject nodeObj,
        IReadOnlyDictionary<string, IReadOnlyList<string>> leavingPortsByNode)
    {
        var primitiveType = GetString(nodeObj, "primitive")!;
        var leavingPorts = leavingPortsByNode.TryGetValue(id, out var ports) ? ports : [];

        var (inputs, outputs) = primitiveType.ToLowerInvariant() switch
        {
            "fork" => (
                (IReadOnlyList<PipelinePortDefinition>)[DataPort("frame")],
                (IReadOnlyList<PipelinePortDefinition>)DerivedOutputs(id, "fork", leavingPorts)),

            "switch" => (
                [DataPort("frame"), ControlPort("class", ClassificationSignalType)],
                DerivedOutputs(id, "switch", leavingPorts)),

            "if" => (
                [DataPort("frame"), ControlPort("productPresent", BooleanGateSignalType)],
                [DataPort("acceptedFrame")]),

            _ => throw new PipelineExpansionException(
                $"Node '{id}' references unknown primitive '{primitiveType}'. Supported: fork, switch, if.")
        };

        return new PipelineNodeDefinition
        {
            Id = id,
            DisplayName = GetString(nodeObj, "displayName") ?? Capitalize(primitiveType),
            Kind = "embedded-primitive",
            Category = "flow-control",
            PrimitiveType = primitiveType,
            ActivationMode = GetString(nodeObj, "activationMode"),
            Backpressure = GetString(nodeObj, "backpressure"),
            Parallelism = GetInt(nodeObj, "parallelism"),
            Config = CloneConfig(nodeObj),
            Inputs = inputs,
            Outputs = outputs
        };
    }

    private static PipelinePortDefinition[] DerivedOutputs(string id, string primitiveType, IReadOnlyList<string> leavingPorts)
    {
        if (leavingPorts.Count == 0)
        {
            throw new PipelineExpansionException(
                $"Primitive '{id}' ({primitiveType}) has no outgoing edges; its output ports are derived from them.");
        }

        return leavingPorts.Select(port => DataPort(port, allowMultipleEdges: true)).ToArray();
    }

    private static (string Category, PipelinePortDefinition[] Inputs, PipelinePortDefinition[] Outputs) StandardModulePorts(
        string nodeId,
        IntegrationCapabilityKind kind) => kind switch
    {
        IntegrationCapabilityKind.Source =>
            ("source", [], [DataPort("frame")]),

        IntegrationCapabilityKind.Processor =>
            ("compute", [DataPort("frame")], [DataPort("frame")]),

        IntegrationCapabilityKind.Classifier =>
            ("classify", [DataPort("frame")], [ControlPort("class", ClassificationSignalType)]),

        IntegrationCapabilityKind.Gate =>
            ("control", [], [ControlPort("productPresent", BooleanGateSignalType)]),

        IntegrationCapabilityKind.Sink =>
            ("output", [DataPort("frame")], []),

        _ => throw new PipelineExpansionException(
            $"Node '{nodeId}' uses module kind '{kind}', which has no lean port mapping. " +
            "Supported kinds: source, processor, classifier, gate, sink.")
    };

    private static ParsedEdge ParseEdge(JsonNode? edgeNode, int index)
    {
        if (edgeNode is not JsonObject edgeObj)
        {
            throw new PipelineExpansionException("Each entry in 'edges' must be a JSON object.");
        }

        var from = edgeObj["from"];
        var to = edgeObj["to"];

        // Rich edge: from/to are objects { nodeId, port }.
        if (from is JsonObject || to is JsonObject)
        {
            var fromRef = from?.Deserialize<PipelinePortReference>(JsonOptions)
                ?? throw new PipelineExpansionException($"Edge #{index + 1} is missing a valid 'from'.");
            var toRef = to?.Deserialize<PipelinePortReference>(JsonOptions)
                ?? throw new PipelineExpansionException($"Edge #{index + 1} is missing a valid 'to'.");

            return new ParsedEdge(
                index, fromRef.NodeId, fromRef.Port, toRef.NodeId, toRef.Port,
                GetString(edgeObj, "kind"), GetString(edgeObj, "id"), GetString(edgeObj, "condition"));
        }

        // Lean edge: from/to are "nodeId.port" strings.
        var (fromNode, fromPort) = SplitPortRef(from?.GetValue<string>(), index, "from");
        var (toNode, toPort) = SplitPortRef(to?.GetValue<string>(), index, "to");

        return new ParsedEdge(
            index, fromNode, fromPort, toNode, toPort,
            GetString(edgeObj, "kind"), GetString(edgeObj, "id"), GetString(edgeObj, "condition"));
    }

    private static (string Node, string Port) SplitPortRef(string? reference, int index, string side)
    {
        if (string.IsNullOrWhiteSpace(reference))
        {
            throw new PipelineExpansionException($"Edge #{index + 1} is missing its '{side}' endpoint.");
        }

        var dot = reference.IndexOf('.');
        if (dot <= 0 || dot == reference.Length - 1)
        {
            throw new PipelineExpansionException(
                $"Edge #{index + 1} '{side}' must be 'nodeId.port' but was '{reference}'.");
        }

        return (reference[..dot], reference[(dot + 1)..]);
    }

    private static JsonObject CloneConfig(JsonObject nodeObj) =>
        nodeObj["config"] is JsonObject config ? (JsonObject)config.DeepClone() : [];

    private static PipelinePortDefinition DataPort(string name, bool allowMultipleEdges = false) =>
        new() { Name = name, Channel = "data", DataType = DataFrameType, AllowMultipleEdges = allowMultipleEdges };

    private static PipelinePortDefinition ControlPort(string name, string dataType) =>
        new() { Name = name, Channel = "control", DataType = dataType };

    private static string? GetString(JsonObject obj, string property) =>
        obj[property] is JsonValue value && value.TryGetValue<string>(out var s) ? s : null;

    private static int? GetInt(JsonObject obj, string property) =>
        obj[property] is JsonValue value && value.TryGetValue<int>(out var i) ? i : null;

    private static string Capitalize(string value) =>
        value.Length == 0 ? value : char.ToUpperInvariant(value[0]) + value[1..];

    private sealed record ParsedEdge(
        int Index,
        string FromNode,
        string FromPort,
        string ToNode,
        string ToPort,
        string? ExplicitKind,
        string? Id,
        string? Condition);
}
