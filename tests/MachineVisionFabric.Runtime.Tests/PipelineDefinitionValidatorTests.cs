using MachineVisionFabric.Contracts.Pipelines;
using MachineVisionFabric.Runtime.Pipelines;

namespace MachineVisionFabric.Runtime.Tests;

public sealed class PipelineDefinitionValidatorTests
{
    [Fact]
    public void Validate_ReturnsValidResult_ForWellFormedGraph()
    {
        var definition = new PipelineDefinition
        {
            Name = "dataset-capture-graph",
            Nodes =
            [
                new PipelineNodeDefinition
                {
                    Id = "camera1",
                    Kind = "integration-module",
                    Category = "source",
                    ModuleId = "mvf.realworld-cognex-camera",
                    Outputs =
                    [
                        new PipelinePortDefinition
                        {
                            Name = "frame",
                            Channel = "data",
                            DataType = "frame"
                        }
                    ]
                },
                new PipelineNodeDefinition
                {
                    Id = "ifProduct",
                    Kind = "embedded-primitive",
                    Category = "flow-control",
                    PrimitiveType = "if",
                    Inputs =
                    [
                        new PipelinePortDefinition
                        {
                            Name = "frame",
                            Channel = "data",
                            DataType = "frame"
                        }
                    ],
                    Outputs =
                    [
                        new PipelinePortDefinition
                        {
                            Name = "acceptedFrame",
                            Channel = "data",
                            DataType = "frame"
                        }
                    ]
                }
            ],
            Edges =
            [
                new PipelineEdgeDefinition
                {
                    Id = "edge-1",
                    Kind = "data",
                    From = new PipelinePortReference
                    {
                        NodeId = "camera1",
                        Port = "frame"
                    },
                    To = new PipelinePortReference
                    {
                        NodeId = "ifProduct",
                        Port = "frame"
                    }
                }
            ]
        };

        var validator = new PipelineDefinitionValidator();
        var result = validator.Validate(definition);

        Assert.True(result.IsValid);
        Assert.Empty(result.Issues);
    }

    [Fact]
    public void Validate_ReturnsError_ForMissingPrimitiveType()
    {
        var definition = new PipelineDefinition
        {
            Nodes =
            [
                new PipelineNodeDefinition
                {
                    Id = "ifProduct",
                    Kind = "embedded-primitive",
                    Category = "flow-control"
                }
            ]
        };

        var validator = new PipelineDefinitionValidator();
        var result = validator.Validate(definition);

        Assert.False(result.IsValid);
        Assert.Contains(result.Issues, issue => issue.Code == "pipeline.node.missing-primitive-type");
    }

    [Fact]
    public void Validate_ReturnsError_ForEdgeDataTypeMismatch()
    {
        var definition = new PipelineDefinition
        {
            Nodes =
            [
                new PipelineNodeDefinition
                {
                    Id = "gate1",
                    Kind = "embedded-primitive",
                    PrimitiveType = "if",
                    Outputs =
                    [
                        new PipelinePortDefinition
                        {
                            Name = "decision",
                            Channel = "control",
                            DataType = "boolean-gate"
                        }
                    ]
                },
                new PipelineNodeDefinition
                {
                    Id = "save1",
                    Kind = "integration-module",
                    ModuleId = "mvf.dataset-writer",
                    Inputs =
                    [
                        new PipelinePortDefinition
                        {
                            Name = "frame",
                            Channel = "data",
                            DataType = "frame"
                        }
                    ]
                }
            ],
            Edges =
            [
                new PipelineEdgeDefinition
                {
                    Id = "edge-1",
                    Kind = "control",
                    From = new PipelinePortReference
                    {
                        NodeId = "gate1",
                        Port = "decision"
                    },
                    To = new PipelinePortReference
                    {
                        NodeId = "save1",
                        Port = "frame"
                    }
                }
            ]
        };

        var validator = new PipelineDefinitionValidator();
        var result = validator.Validate(definition);

        Assert.False(result.IsValid);
        Assert.Contains(result.Issues, issue => issue.Code == "pipeline.edge.target-channel-mismatch");
        Assert.Contains(result.Issues, issue => issue.Code == "pipeline.edge.data-type-mismatch");
    }
}
