using System.Text.Json.Nodes;
using Mvf.Graph.Processing;

namespace Mvf.Abstractions;

/// <summary>
/// The multi-output result of analyzing one frame: a node may emit a derived frame, a routing
/// classification, structured inference metadata, or any combination of them in one cycle.
/// </summary>
public sealed record FrameAnalysisResult(
    IFrameEnvelope? Frame = null,
    FrameClassification? Classification = null,
    JsonNode? Result = null);
