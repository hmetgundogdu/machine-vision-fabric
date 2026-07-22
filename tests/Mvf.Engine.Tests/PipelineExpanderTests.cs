using Mvf.Graph.Integrations;
using Mvf.Engine.Modules;
using Mvf.Engine.Pipelines;

namespace Mvf.Engine.Tests;

public sealed class PipelineExpanderTests
{
    private static readonly IReadOnlyDictionary<string, ModuleManifest> Catalog = new Dictionary<string, ModuleManifest>(StringComparer.OrdinalIgnoreCase)
    {
        ["mvf.cam"] = Manifest("mvf.cam", IntegrationCapabilityKind.Source),
        ["mvf.filter"] = Manifest("mvf.filter", IntegrationCapabilityKind.Processor),
        ["mvf.classify"] = Manifest("mvf.classify", IntegrationCapabilityKind.Classifier),
        ["mvf.gate"] = Manifest("mvf.gate", IntegrationCapabilityKind.Gate),
        ["mvf.sink"] = Manifest("mvf.sink", IntegrationCapabilityKind.Sink)
    };

    private static ModuleManifest Manifest(string id, IntegrationCapabilityKind kind) =>
        new() { Id = id, Name = id, Version = "1.0.0", Kind = kind, Entry = $"{id}.dll" };

    private static PipelineExpander Expander => new();

    [Fact]
    public void Expand_LeanModuleNode_DerivesKindCategoryAndPortsFromCatalog()
    {
        const string json = """
        { "name": "p", "nodes": [ { "id": "cam1", "module": "mvf.cam" } ], "edges": [] }
        """;

        var definition = Expander.Expand(json, Catalog);
        var node = Assert.Single(definition.Nodes);

        Assert.Equal("integration-module", node.Kind);
        Assert.Equal("source", node.Category);
        Assert.Equal("mvf.cam", node.ModuleId);
        Assert.Empty(node.Inputs);
        var output = Assert.Single(node.Outputs);
        Assert.Equal("frame", output.Name);
        Assert.Equal("data", output.Channel);
    }

    [Fact]
    public void Expand_LeanModuleNode_CopiesConfigAndDisplayName()
    {
        const string json = """
        { "nodes": [ { "id": "s1", "module": "mvf.sink", "displayName": "Saver", "config": { "outputRoot": "out" } } ], "edges": [] }
        """;

        var node = Assert.Single(Expander.Expand(json, Catalog).Nodes);

        Assert.Equal("Saver", node.DisplayName);
        Assert.Equal("out", node.Config["outputRoot"]!.GetValue<string>());
    }

    [Fact]
    public void Expand_ForkPrimitive_DerivesOutputsFromLeavingEdges()
    {
        const string json = """
        {
          "nodes": [
            { "id": "cam1", "module": "mvf.cam" },
            { "id": "fork1", "primitive": "fork" },
            { "id": "a", "module": "mvf.sink" },
            { "id": "b", "module": "mvf.sink" }
          ],
          "edges": [
            { "from": "cam1.frame", "to": "fork1.frame" },
            { "from": "fork1.all", "to": "a.frame" },
            { "from": "fork1.dark", "to": "b.frame" }
          ]
        }
        """;

        var fork = Assert.Single(Expander.Expand(json, Catalog).Nodes, n => n.Id == "fork1");

        Assert.Equal("embedded-primitive", fork.Kind);
        Assert.Equal("fork", fork.PrimitiveType);
        Assert.Equal(["frame"], fork.Inputs.Select(p => p.Name));
        Assert.Equal(["all", "dark"], fork.Outputs.Select(p => p.Name));
        Assert.All(fork.Outputs, p => Assert.Equal("data", p.Channel));
    }

    [Fact]
    public void Expand_LeanEdge_DerivesKindFromSourcePortChannel()
    {
        const string json = """
        {
          "nodes": [
            { "id": "cam1", "module": "mvf.cam" },
            { "id": "gate1", "module": "mvf.gate" },
            { "id": "branch1", "primitive": "if" },
            { "id": "s1", "module": "mvf.sink" }
          ],
          "edges": [
            { "from": "cam1.frame", "to": "branch1.frame" },
            { "from": "gate1.productPresent", "to": "branch1.productPresent" },
            { "from": "branch1.acceptedFrame", "to": "s1.frame" }
          ]
        }
        """;

        var edges = Expander.Expand(json, Catalog).Edges;

        Assert.Equal("data", edges.Single(e => e.From.NodeId == "cam1").Kind);
        Assert.Equal("control", edges.Single(e => e.From.NodeId == "gate1").Kind);
        Assert.Equal("data", edges.Single(e => e.From.NodeId == "branch1").Kind);
        // Ids auto-assigned and unique.
        Assert.Equal(["e1", "e2", "e3"], edges.Select(e => e.Id));
    }

    [Fact]
    public void Expand_ClassifierIntoSwitch_ValidatesWithMatchingControlTypes()
    {
        // A full lean graph exercising the perception→control bridge end to end. If the
        // expander derives ports consistently, the real validator must accept it.
        const string json = """
        {
          "name": "classify-route",
          "nodes": [
            { "id": "cam1", "module": "mvf.cam" },
            { "id": "fork1", "primitive": "fork" },
            { "id": "class1", "module": "mvf.classify" },
            { "id": "route1", "primitive": "switch" },
            { "id": "accept", "module": "mvf.sink" },
            { "id": "reject", "module": "mvf.sink" }
          ],
          "edges": [
            { "from": "cam1.frame", "to": "fork1.frame" },
            { "from": "fork1.toClass", "to": "class1.frame" },
            { "from": "fork1.toRoute", "to": "route1.frame" },
            { "from": "class1.class", "to": "route1.class" },
            { "from": "route1.accept", "to": "accept.frame" },
            { "from": "route1.reject", "to": "reject.frame" }
          ]
        }
        """;

        var definition = Expander.Expand(json, Catalog);
        var result = new Mvf.Engine.Pipelines.PipelineDefinitionValidator().Validate(definition);

        Assert.True(result.IsValid, string.Join("; ", result.Issues.Select(i => $"{i.Code}:{i.Message}")));

        var controlEdge = definition.Edges.Single(e => e.From.NodeId == "class1");
        Assert.Equal("control", controlEdge.Kind);
    }

    [Fact]
    public void Expand_UnknownModule_ThrowsExpansionException()
    {
        const string json = """
        { "nodes": [ { "id": "x", "module": "mvf.does-not-exist" } ], "edges": [] }
        """;

        var ex = Assert.Throws<PipelineExpansionException>(() => Expander.Expand(json, Catalog));
        Assert.Contains("mvf.does-not-exist", ex.Message);
    }

    [Fact]
    public void Expand_ForkWithoutLeavingEdges_ThrowsExpansionException()
    {
        const string json = """
        { "nodes": [ { "id": "fork1", "primitive": "fork" } ], "edges": [] }
        """;

        Assert.Throws<PipelineExpansionException>(() => Expander.Expand(json, Catalog));
    }

    [Fact]
    public void Expand_RichNodesAndEdges_PassThroughUnchanged()
    {
        // The rich shape (explicit kind + object edge endpoints) must still round-trip so
        // existing pipelines keep working.
        const string json = """
        {
          "name": "rich",
          "nodes": [
            {
              "id": "cam1", "kind": "integration-module", "category": "source", "moduleId": "mvf.cam",
              "inputs": [], "outputs": [ { "name": "frame", "channel": "data", "dataType": "data/frame" } ]
            },
            {
              "id": "s1", "kind": "integration-module", "category": "output", "moduleId": "mvf.sink",
              "inputs": [ { "name": "frame", "channel": "data", "dataType": "data/frame" } ], "outputs": []
            }
          ],
          "edges": [
            { "id": "e1", "kind": "data", "from": { "nodeId": "cam1", "port": "frame" }, "to": { "nodeId": "s1", "port": "frame" } }
          ]
        }
        """;

        var definition = Expander.Expand(json, Catalog);

        Assert.Equal(2, definition.Nodes.Count);
        var edge = Assert.Single(definition.Edges);
        Assert.Equal("e1", edge.Id);
        Assert.Equal("cam1", edge.From.NodeId);
        Assert.Equal("s1", edge.To.NodeId);
    }

    [Fact]
    public void Load_ModuleCatalog_ReadsManifestsWithoutLoadingAssemblies()
    {
        // The real modules/ tree resolves through the metadata-only catalog.
        var repoRoot = FindRepoRoot();
        var catalog = new ModuleCatalog().Load(Path.Combine(repoRoot, "modules"));

        Assert.True(catalog.ContainsKey("mvf.black-screen-check"));
        Assert.Equal(IntegrationCapabilityKind.Processor, catalog["mvf.black-screen-check"].Kind);
        Assert.Equal("python", catalog["py.brightness-classifier"].Runtime);
    }

    private static string FindRepoRoot()
    {
        var dir = new DirectoryInfo(AppContext.BaseDirectory);
        while (dir is not null && !Directory.Exists(Path.Combine(dir.FullName, "modules")))
        {
            dir = dir.Parent;
        }

        return dir?.FullName ?? throw new InvalidOperationException("Could not locate repo root from test output.");
    }
}
