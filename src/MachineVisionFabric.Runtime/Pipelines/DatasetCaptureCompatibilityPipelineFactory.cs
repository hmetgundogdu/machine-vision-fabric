using System.Text.Json;
using System.Text.Json.Nodes;
using MachineVisionFabric.Contracts.Packages;
using MachineVisionFabric.Contracts.Pipelines;

namespace MachineVisionFabric.Runtime.Pipelines;

public sealed class DatasetCaptureCompatibilityPipelineFactory
{
    public PipelineDefinition Create(FabricProfileManifest manifest, FabricRuntimeProfile profile)
    {
        ArgumentNullException.ThrowIfNull(manifest);
        ArgumentNullException.ThrowIfNull(profile);

        var nodes = new List<PipelineNodeDefinition>();
        var edges = new List<PipelineEdgeDefinition>();

        nodes.Add(CreateSourceNode(profile));
        nodes.Add(CreateGateNode(manifest));
        nodes.Add(CreateBranchNode());

        var branchOutputNodeId = "branch1";
        var branchOutputPort = "acceptedFrame";

        if (string.Equals(manifest.FrameProcessor.Mode, "module", StringComparison.OrdinalIgnoreCase))
        {
            nodes.Add(CreateProcessorNode(manifest));
            edges.Add(CreateDataEdge("edge-processor", branchOutputNodeId, branchOutputPort, "processor1", "frame"));
            branchOutputNodeId = "processor1";
            branchOutputPort = "frame";
        }

        nodes.Add(CreateDatasetWriterNode());

        edges.Add(CreateDataEdge("edge-source-branch", "source1", "frame", "branch1", "frame"));
        edges.Add(CreateControlEdge("edge-gate-branch", "gate1", "productPresent", "branch1", "productPresent"));
        edges.Add(CreateDataEdge("edge-output-sink", branchOutputNodeId, branchOutputPort, "sink1", "frame"));

        return new PipelineDefinition
        {
            Name = $"{manifest.Name}-compatibility-graph",
            Version = manifest.Version,
            Description = $"Synthetic typed graph generated from package '{manifest.Name}'.",
            RuntimeMode = profile.Mode,
            Capabilities = profile.Capabilities,
            Nodes = nodes,
            Edges = edges
        };
    }

    private static PipelineNodeDefinition CreateSourceNode(FabricRuntimeProfile profile)
    {
        if (string.Equals(profile.Source.Mode, "module", StringComparison.OrdinalIgnoreCase))
        {
            return new PipelineNodeDefinition
            {
                Id = "source1",
                DisplayName = "Frame Source",
                Kind = "integration-module",
                Category = "source",
                ModuleId = profile.Source.ModuleId,
                ActivationMode = "resident",
                Config = ToJsonObject(profile.Source.Config),
                Outputs =
                [
                    CreatePort("frame", "data", "frame")
                ]
            };
        }

        return new PipelineNodeDefinition
        {
            Id = "source1",
            DisplayName = "Built-In Simulator Source",
            Kind = "runtime-builtin",
            Category = "source",
            BuiltinType = "folder-sequence-source",
            ActivationMode = "resident",
            Outputs =
            [
                CreatePort("frame", "data", "frame")
            ]
        };
    }

    private static PipelineNodeDefinition CreateGateNode(FabricProfileManifest manifest)
    {
        if (string.Equals(manifest.ProductPresenceGate.Mode, "module", StringComparison.OrdinalIgnoreCase))
        {
            return new PipelineNodeDefinition
            {
                Id = "gate1",
                DisplayName = "Product Presence Gate",
                Kind = "integration-module",
                Category = "control",
                ModuleId = manifest.ProductPresenceGate.ModuleId,
                ActivationMode = "resident",
                Config = ToJsonObject(manifest.ProductPresenceGate.Config),
                Outputs =
                [
                    CreatePort("productPresent", "control", "boolean-gate")
                ]
            };
        }

        return new PipelineNodeDefinition
        {
            Id = "gate1",
            DisplayName = "Built-In Product Presence Gate",
            Kind = "runtime-builtin",
            Category = "control",
            BuiltinType = "simulated-product-presence-gate",
            ActivationMode = "resident",
            Outputs =
            [
                CreatePort("productPresent", "control", "boolean-gate")
            ]
        };
    }

    private static PipelineNodeDefinition CreateBranchNode()
    {
        return new PipelineNodeDefinition
        {
            Id = "branch1",
            DisplayName = "If Product Present",
            Kind = "embedded-primitive",
            Category = "flow-control",
            PrimitiveType = "if",
            ActivationMode = "resident",
            Inputs =
            [
                CreatePort("frame", "data", "frame"),
                CreatePort("productPresent", "control", "boolean-gate")
            ],
            Outputs =
            [
                CreatePort("acceptedFrame", "data", "frame")
            ]
        };
    }

    private static PipelineNodeDefinition CreateProcessorNode(FabricProfileManifest manifest)
    {
        return new PipelineNodeDefinition
        {
            Id = "processor1",
            DisplayName = "Frame Processor",
            Kind = "integration-module",
            Category = "compute",
            ModuleId = manifest.FrameProcessor.ModuleId,
            ActivationMode = "resident",
            Config = ToJsonObject(manifest.FrameProcessor.Config),
            Inputs =
            [
                CreatePort("frame", "data", "frame")
            ],
            Outputs =
            [
                CreatePort("frame", "data", "frame")
            ]
        };
    }

    private static PipelineNodeDefinition CreateDatasetWriterNode()
    {
        return new PipelineNodeDefinition
        {
            Id = "sink1",
            DisplayName = "Dataset Writer",
            Kind = "runtime-builtin",
            Category = "output",
            BuiltinType = "dataset-writer",
            ActivationMode = "resident",
            Inputs =
            [
                CreatePort("frame", "data", "frame")
            ]
        };
    }

    private static PipelinePortDefinition CreatePort(string name, string channel, string dataType)
    {
        return new PipelinePortDefinition
        {
            Name = name,
            Channel = channel,
            DataType = dataType,
            Required = true,
            AllowMultipleEdges = false
        };
    }

    private static PipelineEdgeDefinition CreateDataEdge(string id, string fromNode, string fromPort, string toNode, string toPort)
    {
        return new PipelineEdgeDefinition
        {
            Id = id,
            Kind = "data",
            From = new PipelinePortReference
            {
                NodeId = fromNode,
                Port = fromPort
            },
            To = new PipelinePortReference
            {
                NodeId = toNode,
                Port = toPort
            }
        };
    }

    private static PipelineEdgeDefinition CreateControlEdge(string id, string fromNode, string fromPort, string toNode, string toPort)
    {
        return new PipelineEdgeDefinition
        {
            Id = id,
            Kind = "control",
            From = new PipelinePortReference
            {
                NodeId = fromNode,
                Port = fromPort
            },
            To = new PipelinePortReference
            {
                NodeId = toNode,
                Port = toPort
            }
        };
    }

    private static JsonObject ToJsonObject(JsonElement element)
    {
        if (element.ValueKind is JsonValueKind.Undefined or JsonValueKind.Null)
        {
            return [];
        }

        return JsonNode.Parse(element.GetRawText()) as JsonObject ?? [];
    }
}
