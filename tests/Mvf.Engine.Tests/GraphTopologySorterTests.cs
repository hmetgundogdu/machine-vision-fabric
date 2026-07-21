using Mvf.Graph.Pipelines;
using Mvf.Engine.Execution;

namespace Mvf.Engine.Tests;

public sealed class GraphTopologySorterTests
{
    [Fact]
    public void Sort_ReturnsSourceBeforeSink_ForLinearChain()
    {
        var definition = new PipelineDefinition
        {
            Name = "test",
            Nodes =
            [
                new PipelineNodeDefinition { Id = "source1", Kind = "integration-module", Category = "source" },
                new PipelineNodeDefinition { Id = "sink1", Kind = "runtime-builtin", Category = "output" }
            ],
            Edges =
            [
                new PipelineEdgeDefinition
                {
                    Id = "e1",
                    Kind = "data",
                    From = new PipelinePortReference { NodeId = "source1", Port = "frame" },
                    To = new PipelinePortReference { NodeId = "sink1", Port = "frame" }
                }
            ]
        };

        var order = GraphTopologySorter.Sort(definition);

        Assert.Equal(2, order.Count);
        Assert.Equal("source1", order[0].Id);
        Assert.Equal("sink1", order[1].Id);
    }

    [Fact]
    public void Sort_HandlesGateAndBranch_InCorrectOrder()
    {
        // source1 → branch1, gate1 → branch1, branch1 → sink1
        var definition = new PipelineDefinition
        {
            Name = "test",
            Nodes =
            [
                new PipelineNodeDefinition { Id = "source1", Kind = "integration-module", Category = "source" },
                new PipelineNodeDefinition { Id = "gate1", Kind = "integration-module", Category = "control" },
                new PipelineNodeDefinition { Id = "branch1", Kind = "embedded-primitive", Category = "flow-control", PrimitiveType = "if" },
                new PipelineNodeDefinition { Id = "sink1", Kind = "runtime-builtin", Category = "output" }
            ],
            Edges =
            [
                new PipelineEdgeDefinition
                {
                    Id = "e1", Kind = "data",
                    From = new PipelinePortReference { NodeId = "source1", Port = "frame" },
                    To = new PipelinePortReference { NodeId = "branch1", Port = "frame" }
                },
                new PipelineEdgeDefinition
                {
                    Id = "e2", Kind = "control",
                    From = new PipelinePortReference { NodeId = "gate1", Port = "productPresent" },
                    To = new PipelinePortReference { NodeId = "branch1", Port = "productPresent" }
                },
                new PipelineEdgeDefinition
                {
                    Id = "e3", Kind = "data",
                    From = new PipelinePortReference { NodeId = "branch1", Port = "acceptedFrame" },
                    To = new PipelinePortReference { NodeId = "sink1", Port = "frame" }
                }
            ]
        };

        var order = GraphTopologySorter.Sort(definition);

        Assert.Equal(4, order.Count);

        // source1 and gate1 have no upstream dependencies — they come before branch1
        var branchIndex = order.Select((n, i) => (n, i)).First(x => x.n.Id == "branch1").i;
        var sinkIndex = order.Select((n, i) => (n, i)).First(x => x.n.Id == "sink1").i;
        var sourceIndex = order.Select((n, i) => (n, i)).First(x => x.n.Id == "source1").i;
        var gateIndex = order.Select((n, i) => (n, i)).First(x => x.n.Id == "gate1").i;

        Assert.True(sourceIndex < branchIndex);
        Assert.True(gateIndex < branchIndex);
        Assert.True(branchIndex < sinkIndex);
    }

    [Fact]
    public void Sort_ThrowsOnCyclicGraph()
    {
        var definition = new PipelineDefinition
        {
            Name = "cyclic",
            Nodes =
            [
                new PipelineNodeDefinition { Id = "a", Kind = "integration-module", Category = "source" },
                new PipelineNodeDefinition { Id = "b", Kind = "integration-module", Category = "compute" }
            ],
            Edges =
            [
                new PipelineEdgeDefinition
                {
                    Id = "e1", Kind = "data",
                    From = new PipelinePortReference { NodeId = "a", Port = "out" },
                    To = new PipelinePortReference { NodeId = "b", Port = "in" }
                },
                new PipelineEdgeDefinition
                {
                    Id = "e2", Kind = "data",
                    From = new PipelinePortReference { NodeId = "b", Port = "out" },
                    To = new PipelinePortReference { NodeId = "a", Port = "in" }
                }
            ]
        };

        var ex = Assert.Throws<InvalidOperationException>(() => GraphTopologySorter.Sort(definition));
        Assert.Contains("cycle", ex.Message, StringComparison.OrdinalIgnoreCase);
    }
}
