using Mvf.Graph.Integrations;
using Mvf.Engine.Modules;
using Mvf.Engine.Pipelines;

namespace Mvf.Engine.Tests;

public sealed class PipelineExpanderTests
{
    private static readonly IReadOnlyDictionary<string, ModuleCatalogEntry> Catalog = new Dictionary<string, ModuleCatalogEntry>(StringComparer.OrdinalIgnoreCase)
    {
        ["mvf.cam"] = Entry("mvf.cam", IntegrationCapabilityKind.Source),
        ["mvf.filter"] = Entry("mvf.filter", IntegrationCapabilityKind.Processor),
        ["mvf.classify"] = Entry("mvf.classify", IntegrationCapabilityKind.Classifier),
        ["mvf.gate"] = Entry("mvf.gate", IntegrationCapabilityKind.Gate),
        ["mvf.sink"] = Entry("mvf.sink", IntegrationCapabilityKind.Sink)
    };

    private static ModuleCatalogEntry Entry(string id, IntegrationCapabilityKind kind) =>
        new(new ModuleManifest { Id = id, Name = id, Version = "1.0.0", Kind = kind, Entry = $"{id}.dll" }, Directory: id);

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
    public void Expand_ValuePrimitive_HasNoInputsAndOneTypedOutput()
    {
        const string json = """
        { "nodes": [ { "id": "threshold", "primitive": "value",
                       "config": { "type": "int", "literal": 40 } } ], "edges": [] }
        """;

        var node = Assert.Single(Expander.Expand(json, Catalog).Nodes);

        Assert.Equal("embedded-primitive", node.Kind);
        Assert.Equal("value", node.Category);
        Assert.Empty(node.Inputs);
        var output = Assert.Single(node.Outputs);
        Assert.Equal("value", output.Name);
        Assert.Equal("control", output.Channel);
        Assert.Equal("control/value:int", output.DataType);
    }

    [Fact]
    public void Expand_ValuePrimitive_DefaultsToString()
    {
        const string json = """
        { "nodes": [ { "id": "folder", "primitive": "value", "config": { "literal": "out" } } ], "edges": [] }
        """;

        var node = Assert.Single(Expander.Expand(json, Catalog).Nodes);

        Assert.Equal("control/value:string", node.Outputs[0].DataType);
    }

    [Fact]
    public void Expand_ValuePrimitive_ListShape_ProducesACollectionPort()
    {
        const string json = """
        { "nodes": [ { "id": "cameras", "primitive": "value",
                       "config": { "type": "json", "shape": "list", "literal": [] } } ], "edges": [] }
        """;

        var node = Assert.Single(Expander.Expand(json, Catalog).Nodes);

        Assert.Equal("control/list:json", Assert.Single(node.Outputs).DataType);
    }

    [Fact]
    public void Expand_ListValueIntoSelectItems_TypesLineUp()
    {
        const string json = """
        {
          "nodes": [
            { "id": "cameras", "primitive": "value", "config": { "type": "json", "shape": "list", "literal": [] } },
            { "id": "pick", "primitive": "select", "config": { "mode": "one", "by": "serial" } }
          ],
          "edges": [ { "from": "cameras.value", "to": "pick.items" } ]
        }
        """;

        var definition = Expander.Expand(json, Catalog);
        var cameras = definition.Nodes.Single(n => n.Id == "cameras");
        var pick = definition.Nodes.Single(n => n.Id == "pick");

        Assert.Equal(cameras.Outputs[0].DataType, pick.Inputs.Single(p => p.Name == "items").DataType);
        Assert.Equal("control", Assert.Single(definition.Edges).Kind);
    }

    [Fact]
    public void Expand_SelectPrimitive_ModeOne_NarrowsAListToAValue()
    {
        const string json = """
        { "nodes": [ { "id": "pickCam", "primitive": "select",
                       "config": { "mode": "one", "by": "serial" } } ], "edges": [] }
        """;

        var node = Assert.Single(Expander.Expand(json, Catalog).Nodes);

        var items = node.Inputs.Single(p => p.Name == "items");
        var criterion = node.Inputs.Single(p => p.Name == "criterion");
        var selected = Assert.Single(node.Outputs);

        Assert.Equal("control/list:json", items.DataType);
        Assert.True(items.Required);
        Assert.Equal("control/value:json", criterion.DataType);
        Assert.False(criterion.Required);
        Assert.Equal("control/value:json", selected.DataType);
    }

    [Fact]
    public void Expand_SelectPrimitive_ModeMany_NarrowsAListToAList()
    {
        const string json = """
        { "nodes": [ { "id": "bigOnes", "primitive": "select",
                       "config": { "mode": "many", "type": "int" } } ], "edges": [] }
        """;

        var node = Assert.Single(Expander.Expand(json, Catalog).Nodes);

        Assert.Equal("control/list:int", node.Inputs.Single(p => p.Name == "items").DataType);
        Assert.Equal("control/list:int", Assert.Single(node.Outputs).DataType);
    }

    [Fact]
    public void Expand_ValueIntoSelect_ProducesAControlEdge()
    {
        const string json = """
        {
          "nodes": [
            { "id": "serial", "primitive": "value", "config": { "type": "string", "literal": "ABC123" } },
            { "id": "pickCam", "primitive": "select", "config": { "mode": "one", "type": "string" } }
          ],
          "edges": [ { "from": "serial.value", "to": "pickCam.criterion" } ]
        }
        """;

        var edge = Assert.Single(Expander.Expand(json, Catalog).Edges);

        Assert.Equal("control", edge.Kind);
    }

    [Fact]
    public void Expand_UnknownValueType_IsCarriedThroughForTheValidatorToReport()
    {
        const string json = """
        { "nodes": [ { "id": "odd", "primitive": "value",
                       "config": { "type": "decimal", "literal": 1 } } ], "edges": [] }
        """;

        var node = Assert.Single(Expander.Expand(json, Catalog).Nodes);

        Assert.Equal("control/value:decimal", Assert.Single(node.Outputs).DataType);
    }

    [Fact]
    public void Expand_LoopPrimitive_IsPortless()
    {
        const string json = """
        { "nodes": [ { "id": "cycle", "primitive": "loop",
                       "config": { "mode": "until-exhausted" } } ], "edges": [] }
        """;

        var node = Assert.Single(Expander.Expand(json, Catalog).Nodes);

        Assert.Equal("embedded-primitive", node.Kind);
        Assert.Equal("flow-control", node.Category);
        Assert.Equal("loop", node.PrimitiveType);

        // The loop moves no value — it owns iteration and pause, so it carries no ports.
        Assert.Empty(node.Inputs);
        Assert.Empty(node.Outputs);
    }

    [Fact]
    public void Expand_LoopBackEdge_ClosesByNodeId_NoPorts()
    {
        // The tail closes the loop with a plain id-level edge (`save -> cycle`) — no port, because it moves
        // no value. The data plane (cam -> save) stays a strict DAG.
        const string json = """
        {
          "nodes": [
            { "id": "cam",   "module": "mvf.cam" },
            { "id": "save",  "module": "mvf.sink" },
            { "id": "cycle", "primitive": "loop", "config": { "mode": "forever" } }
          ],
          "edges": [
            { "from": "cam.frame", "to": "save.frame" },
            { "from": "save",      "to": "cycle" }
          ]
        }
        """;

        var definition = Expander.Expand(json, Catalog);
        var result = new Mvf.Engine.Pipelines.PipelineDefinitionValidator().Validate(definition);

        Assert.True(result.IsValid, string.Join("; ", result.Issues.Select(i => $"{i.Code}:{i.Message}")));

        var closing = definition.Edges.Single(e => e.To.NodeId == "cycle");
        Assert.Equal("loop", closing.Kind);
        Assert.Equal("save", closing.From.NodeId);
        Assert.Equal(string.Empty, closing.From.Port);
        Assert.Equal(string.Empty, closing.To.Port);
    }

    [Fact]
    public void Load_ModuleCatalog_ReadsManifestsWithoutLoadingAssemblies()
    {
        // The real modules/ tree resolves through the metadata-only catalog.
        var repoRoot = FindRepoRoot();
        var catalog = new ModuleCatalog().Load(Path.Combine(repoRoot, "modules"));

        Assert.True(catalog.ContainsKey("mvf.black-screen-check"));
        Assert.Equal(IntegrationCapabilityKind.Processor, catalog["mvf.black-screen-check"].Manifest.Kind);
        Assert.Equal("python", catalog["py.brightness-classifier"].Manifest.Runtime);
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
