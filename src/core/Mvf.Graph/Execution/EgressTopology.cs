using Mvf.Graph.Pipelines;

namespace Mvf.Graph.Execution;

/// <summary>
/// The shape of the running graph, published once per run so a remote observer can draw the pipeline
/// instead of inferring a flat node list from the transitions it happens to see.
///
/// <para><b>Why a projection and not the <see cref="PipelineDefinition"/> itself:</b> egress is announced
/// on the LAN by the alive-beacon and served to whoever attaches, so everything on this wire is
/// effectively public within the plant network. A node's <c>Config</c> routinely carries camera
/// credentials, PLC addresses and filesystem paths, and none of that is needed to render a graph. This
/// record therefore carries <b>structure only</b> — identity, kind, ports and edges. Adding a field here
/// means deciding it is safe to broadcast.</para>
///
/// <para>The field set is exactly what rebuilding a renderable <see cref="PipelineDefinition"/> requires
/// (see <see cref="ToDefinition"/>), so the observer reuses the local graph renderer unchanged.</para>
/// </summary>
public sealed record EgressTopology
{
    /// <summary>The pipeline's display name (<see cref="PipelineDefinition.Name"/>).</summary>
    public string Name { get; init; } = string.Empty;

    /// <summary>The run this topology belongs to. Carried here rather than as a separate sink argument so
    /// a replayed topology stays self-describing — an observer keys its state by run id.</summary>
    public string RunId { get; init; } = string.Empty;

    public IReadOnlyList<EgressTopologyNode> Nodes { get; init; } = [];

    public IReadOnlyList<EgressTopologyEdge> Edges { get; init; } = [];

    /// <summary>Projects the structure of a definition, dropping config and everything else not needed to
    /// draw the graph.</summary>
    public static EgressTopology FromDefinition(PipelineDefinition definition, string runId)
    {
        ArgumentNullException.ThrowIfNull(definition);

        return new EgressTopology
        {
            Name = definition.Name,
            RunId = runId,
            Nodes = [.. definition.Nodes.Select(n => new EgressTopologyNode
            {
                Id = n.Id,
                DisplayName = n.DisplayName,
                Kind = n.Kind,
                Category = n.Category,
                // The renderer's type label is derived from whichever of these matches Kind; carrying all
                // three keeps that derivation identical on the observer side.
                ModuleId = n.ModuleId,
                PrimitiveType = n.PrimitiveType,
                BuiltinType = n.BuiltinType,
                Inputs = [.. n.Inputs.Select(EgressTopologyPort.FromDefinition)],
                Outputs = [.. n.Outputs.Select(EgressTopologyPort.FromDefinition)],
            })],
            Edges = [.. definition.Edges.Select(e => new EgressTopologyEdge
            {
                Id = e.Id,
                Kind = e.Kind,
                FromNode = e.From.NodeId,
                FromPort = e.From.Port,
                ToNode = e.To.NodeId,
                ToPort = e.To.Port,
            })],
        };
    }

    /// <summary>Rebuilds a definition carrying this structure — enough for layout and rendering, with
    /// empty config. Consumer-side; never fed back into an executor.</summary>
    public PipelineDefinition ToDefinition() => new()
    {
        Name = Name,
        Nodes = [.. Nodes.Select(n => new PipelineNodeDefinition
        {
            Id = n.Id,
            DisplayName = n.DisplayName,
            Kind = n.Kind,
            Category = n.Category,
            ModuleId = n.ModuleId,
            PrimitiveType = n.PrimitiveType,
            BuiltinType = n.BuiltinType,
            Inputs = [.. n.Inputs.Select(p => p.ToDefinition())],
            Outputs = [.. n.Outputs.Select(p => p.ToDefinition())],
        })],
        Edges = [.. Edges.Select(e => new PipelineEdgeDefinition
        {
            Id = e.Id,
            Kind = e.Kind,
            From = new PipelinePortReference { NodeId = e.FromNode, Port = e.FromPort },
            To = new PipelinePortReference { NodeId = e.ToNode, Port = e.ToPort },
        })],
    };
}

/// <summary>One node's identity and ports — no config.</summary>
public sealed record EgressTopologyNode
{
    public string Id { get; init; } = string.Empty;

    public string DisplayName { get; init; } = string.Empty;

    /// <summary>integration-module | embedded-primitive | runtime-builtin.</summary>
    public string Kind { get; init; } = string.Empty;

    /// <summary>source | compute | classify | flow | sink | value — drives the node's colour.</summary>
    public string Category { get; init; } = string.Empty;

    public string? ModuleId { get; init; }

    public string? PrimitiveType { get; init; }

    public string? BuiltinType { get; init; }

    public IReadOnlyList<EgressTopologyPort> Inputs { get; init; } = [];

    public IReadOnlyList<EgressTopologyPort> Outputs { get; init; } = [];
}

/// <summary>A typed port: the data/control split is part of the structure, so it travels too.</summary>
public sealed record EgressTopologyPort
{
    public string Name { get; init; } = string.Empty;

    /// <summary>data | control.</summary>
    public string Channel { get; init; } = "data";

    public string DataType { get; init; } = string.Empty;

    public static EgressTopologyPort FromDefinition(PipelinePortDefinition port) => new()
    {
        Name = port.Name,
        Channel = port.Channel,
        DataType = port.DataType,
    };

    public PipelinePortDefinition ToDefinition() => new()
    {
        Name = Name,
        Channel = Channel,
        DataType = DataType,
    };
}

/// <summary>A directed edge, keeping its data/control kind.</summary>
public sealed record EgressTopologyEdge
{
    public string Id { get; init; } = string.Empty;

    /// <summary>data | control.</summary>
    public string Kind { get; init; } = "data";

    public string FromNode { get; init; } = string.Empty;

    public string FromPort { get; init; } = string.Empty;

    public string ToNode { get; init; } = string.Empty;

    public string ToPort { get; init; } = string.Empty;
}
