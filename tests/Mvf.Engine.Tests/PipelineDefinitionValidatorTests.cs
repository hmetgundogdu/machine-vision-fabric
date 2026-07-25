using Mvf.Graph.Pipelines;
using Mvf.Engine.Modules;
using Mvf.Engine.Pipelines;

namespace Mvf.Engine.Tests;

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

    // ── value / select ────────────────────────────────────────────────────────────────────────────

    private static PipelineValidationResult ValidateLean(string json) =>
        new PipelineDefinitionValidator().Validate(
            new PipelineExpander().Expand(
                json, new Dictionary<string, ModuleCatalogEntry>(StringComparer.OrdinalIgnoreCase)));

    [Fact]
    public void Validate_AcceptsAWellFormedValueNode()
    {
        var result = ValidateLean("""
        { "nodes": [ { "id": "threshold", "primitive": "value",
                       "config": { "type": "int", "literal": 40 } } ], "edges": [] }
        """);

        Assert.True(result.IsValid);
    }

    [Fact]
    public void Validate_RejectsAnUnknownValueType()
    {
        var result = ValidateLean("""
        { "nodes": [ { "id": "odd", "primitive": "value",
                       "config": { "type": "decimal", "literal": 1 } } ], "edges": [] }
        """);

        Assert.Contains(result.Issues, i => i.Code == "pipeline.node.invalid-value-type" && i.NodeId == "odd");
    }

    [Fact]
    public void Validate_RejectsASchemaThatIsNotASchema()
    {
        var result = ValidateLean("""
        { "nodes": [ { "id": "camera", "primitive": "value",
                       "config": { "type": "json", "binding": "cam", "schema": { "type": "recordset" } } } ],
          "edges": [] }
        """);

        Assert.Contains(result.Issues, i => i.Code == "pipeline.node.invalid-schema");
    }

    [Fact]
    public void Validate_RejectsALiteralOfTheWrongType()
    {
        var result = ValidateLean("""
        { "nodes": [ { "id": "threshold", "primitive": "value",
                       "config": { "type": "int", "literal": "forty" } } ], "edges": [] }
        """);

        Assert.Contains(result.Issues, i => i.Code == "pipeline.node.literal-type-mismatch");
    }

    [Fact]
    public void Validate_RejectsALiteralThatBreaksItsSchema()
    {
        var result = ValidateLean("""
        {
          "nodes": [
            { "id": "camera", "primitive": "value",
              "config": {
                "type": "json",
                "literal": { "address": "10.0.0.4" },
                "schema": { "type": "object", "required": ["serial"] }
              } }
          ],
          "edges": []
        }
        """);

        Assert.Contains(result.Issues, i => i.Code == "pipeline.node.literal-type-mismatch");
    }

    [Fact]
    public void Validate_RejectsAValueNodeThatCanNeverProduceAValue()
    {
        var result = ValidateLean("""
        { "nodes": [ { "id": "orphan", "primitive": "value", "config": { "type": "string" } } ], "edges": [] }
        """);

        Assert.Contains(result.Issues, i => i.Code == "pipeline.node.unresolvable-value" && i.NodeId == "orphan");
    }

    [Fact]
    public void Validate_AcceptsAValueNodeThatOnlyDeclaresABinding()
    {
        var result = ValidateLean("""
        { "nodes": [ { "id": "cam", "primitive": "value",
                       "config": { "type": "string", "binding": "camera.serial" } } ], "edges": [] }
        """);

        Assert.True(result.IsValid);
    }

    [Fact]
    public void Validate_RejectsAnUnknownValueShape()
    {
        var result = ValidateLean("""
        { "nodes": [ { "id": "odd", "primitive": "value",
                       "config": { "type": "int", "shape": "bag", "literal": 1 } } ], "edges": [] }
        """);

        Assert.Contains(result.Issues, i => i.Code == "pipeline.node.invalid-value-shape" && i.NodeId == "odd");
    }

    [Fact]
    public void Validate_RejectsAListLiteralWithAMistypedElement()
    {
        var result = ValidateLean("""
        { "nodes": [ { "id": "sizes", "primitive": "value",
                       "config": { "type": "int", "shape": "list", "literal": [1, 2, "three"] } } ], "edges": [] }
        """);

        Assert.Contains(result.Issues, i => i.Code == "pipeline.node.literal-type-mismatch");
    }

    [Fact]
    public void Validate_AppliesTheSchemaToEachElementOfAListLiteral()
    {
        var result = ValidateLean("""
        {
          "nodes": [
            { "id": "cameras", "primitive": "value",
              "config": {
                "type": "json",
                "shape": "list",
                "literal": [ { "serial": "A" }, { "protocol": "gige" } ],
                "schema": { "type": "object", "required": ["serial"] }
              } }
          ],
          "edges": []
        }
        """);

        Assert.Contains(result.Issues, i => i.Code == "pipeline.node.literal-type-mismatch");
    }

    [Fact]
    public void Validate_AcceptsAListLiteralWhereEveryElementFitsTheSchema()
    {
        var result = ValidateLean("""
        {
          "nodes": [
            { "id": "cameras", "primitive": "value",
              "config": {
                "type": "json",
                "shape": "list",
                "literal": [ { "serial": "A" }, { "serial": "B" } ],
                "schema": { "type": "object", "required": ["serial"] }
              } }
          ],
          "edges": []
        }
        """);

        Assert.True(result.IsValid, string.Join("; ", result.Issues.Select(i => i.Message)));
    }

    [Fact]
    public void Validate_RejectsAnUnknownSelectMode()
    {
        var result = ValidateLean("""
        { "nodes": [ { "id": "pick", "primitive": "select", "config": { "mode": "first" } } ], "edges": [] }
        """);

        Assert.Contains(result.Issues, i => i.Code == "pipeline.node.select-invalid-mode" && i.NodeId == "pick");
    }

    [Fact]
    public void Validate_RejectsASelectWhoseCriterionIsNotTheElementType()
    {
        // Hand-written rather than expanded: the expander always derives both ports from one declared
        // type, so this can only drift apart in a rich definition.
        var definition = new PipelineDefinition
        {
            Name = "drifted",
            Nodes =
            [
                new PipelineNodeDefinition
                {
                    Id = "pick",
                    Kind = "embedded-primitive",
                    Category = "value",
                    PrimitiveType = "select",
                    Inputs =
                    [
                        new PipelinePortDefinition { Name = "items", Channel = "control", DataType = "control/list:json" },
                        new PipelinePortDefinition { Name = "criterion", Channel = "control", DataType = "control/value:int", Required = false }
                    ],
                    Outputs =
                    [
                        new PipelinePortDefinition { Name = "selected", Channel = "control", DataType = "control/value:json" }
                    ]
                }
            ]
        };

        var result = new PipelineDefinitionValidator().Validate(definition);

        Assert.Contains(result.Issues, i => i.Code == "pipeline.node.select-type-mismatch");
    }

    [Fact]
    public void Validate_RejectsAValueEdgeIntoAMismatchedElementType()
    {
        var result = ValidateLean("""
        {
          "nodes": [
            { "id": "serial", "primitive": "value", "config": { "type": "string", "literal": "ABC" } },
            { "id": "pick", "primitive": "select", "config": { "mode": "one", "type": "json" } }
          ],
          "edges": [ { "from": "serial.value", "to": "pick.criterion" } ]
        }
        """);

        Assert.Contains(result.Issues, i => i.Code == "pipeline.edge.data-type-mismatch");
    }

    [Fact]
    public void Validate_AcceptsAWellFormedLoopNode()
    {
        var result = ValidateLean("""
        { "nodes": [ { "id": "cycle", "primitive": "loop",
                       "config": { "mode": "until-exhausted" } } ], "edges": [] }
        """);

        Assert.True(result.IsValid, string.Join("; ", result.Issues.Select(i => $"{i.Code}: {i.Message}")));
    }

    [Fact]
    public void Validate_AcceptsALoopWithNoModeAsUntilExhausted()
    {
        var result = ValidateLean("""
        { "nodes": [ { "id": "cycle", "primitive": "loop" } ], "edges": [] }
        """);

        Assert.True(result.IsValid, string.Join("; ", result.Issues.Select(i => $"{i.Code}: {i.Message}")));
    }

    [Fact]
    public void Validate_RejectsAnUnknownLoopMode()
    {
        var result = ValidateLean("""
        { "nodes": [ { "id": "cycle", "primitive": "loop", "config": { "mode": "spin" } } ], "edges": [] }
        """);

        Assert.Contains(result.Issues, i => i.Code == "pipeline.node.loop-invalid-mode" && i.NodeId == "cycle");
    }

    [Fact]
    public void Validate_AcceptsForeverAndCountModes()
    {
        Assert.True(ValidateLean("""
        { "nodes": [ { "id": "cycle", "primitive": "loop", "config": { "mode": "forever" } } ], "edges": [] }
        """).IsValid);

        Assert.True(ValidateLean("""
        { "nodes": [ { "id": "cycle", "primitive": "loop", "config": { "mode": "count", "count": 100 } } ], "edges": [] }
        """).IsValid);
    }

    [Fact]
    public void Validate_RejectsCountModeWithoutAPositiveCount()
    {
        // A loop told to stop after N cycles but never told N would never stop — catch it statically.
        var result = ValidateLean("""
        { "nodes": [ { "id": "cycle", "primitive": "loop", "config": { "mode": "count" } } ], "edges": [] }
        """);

        Assert.Contains(result.Issues, i => i.Code == "pipeline.node.loop-missing-count" && i.NodeId == "cycle");
    }

    [Fact]
    public void Validate_AcceptsAWellFormedModuleBinding()
    {
        var result = ValidateLean("""
        { "nodes": [ { "id": "cam", "kind": "runtime-builtin", "builtinType": "folder-sequence-source",
                       "config": { "frameIntervalMs": 300 },
                       "bindings": { "frameIntervalMs": { "type": "int",
                                     "schema": { "type": "integer", "minimum": 0 } } } } ],
          "edges": [] }
        """);

        Assert.True(result.IsValid, string.Join("; ", result.Issues.Select(i => $"{i.Code}: {i.Message}")));
    }

    [Fact]
    public void Validate_RejectsAnUnknownBindingType()
    {
        var result = ValidateLean("""
        { "nodes": [ { "id": "cam", "kind": "runtime-builtin", "builtinType": "folder-sequence-source",
                       "bindings": { "frameIntervalMs": { "type": "decimal" } } } ],
          "edges": [] }
        """);

        Assert.Contains(result.Issues, i => i.Code == "pipeline.node.invalid-binding-type" && i.NodeId == "cam");
    }

    [Fact]
    public void Validate_RejectsAnInvalidBindingSchema()
    {
        var result = ValidateLean("""
        { "nodes": [ { "id": "cam", "kind": "runtime-builtin", "builtinType": "folder-sequence-source",
                       "bindings": { "frameIntervalMs": { "type": "int", "schema": { "type": "recordset" } } } } ],
          "edges": [] }
        """);

        Assert.Contains(result.Issues, i => i.Code == "pipeline.node.invalid-binding-schema" && i.NodeId == "cam");
    }
}
