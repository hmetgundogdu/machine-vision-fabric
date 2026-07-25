using Mvf.Graph.Integrations;
using Mvf.Graph.Processing;
using Mvf.Abstractions;
using Mvf.Sdk;

namespace Mvf.Example.BrightnessGate;

/// <summary>
/// Minimal <b>.NET SDK example module</b>: a processor that accepts a frame only when its
/// mean byte value meets a threshold — a dependency-free "brightness gate".
///
/// It mirrors the Python (<c>modules/py-invert-transformer</c>) and C++
/// (<c>src/sdk/cpp/examples/invert_transformer.cpp</c>) examples, but for the in-process
/// .NET module contract: derive <see cref="FrameProcessorModuleBase{TOptions}"/>, describe the
/// node's typed ports via <see cref="IntegrationModuleDescriptorBuilder"/>, and implement an
/// <see cref="IFrameProcessor"/> that returns an accept/reject <see cref="FrameProcessorDecision"/>.
///
/// Node category: compute · Input: frame · Output: frame (passed through only when accepted).
/// </summary>
public sealed class BrightnessGateModule : FrameProcessorModuleBase<BrightnessGateOptions>
{
    protected override IntegrationModuleDescriptor BuildDescriptor() =>
        IntegrationModuleDescriptorBuilder.CreateProcessor<BrightnessGateOptions>(
            moduleId:       "mvf.example-brightness-gate",
            displayName:    "Brightness Gate (.NET example)",
            version:        "1.0.0",
            capabilityName: "brightness-gate",
            description:    "Example .NET module: accepts a frame only when its mean byte value is at or above a configurable threshold.");

    protected override IFrameProcessor CreateProcessor(BrightnessGateOptions options) =>
        new BrightnessGateProcessor(options);

    private sealed class BrightnessGateProcessor(BrightnessGateOptions options) : IFrameProcessor
    {
        public async Task<FrameProcessorDecision> EvaluateAsync(
            IFrameEnvelope frame, CancellationToken cancellationToken)
        {
            // Read the raw frame bytes from the envelope (no image decode — dependency-free).
            await using var source = await frame.OpenReadAsync(cancellationToken);
            using var buffer = new MemoryStream();
            await source.CopyToAsync(buffer, cancellationToken);
            var bytes = buffer.ToArray();

            var mean = bytes.Length == 0 ? 0.0 : bytes.Average(b => (double)b);
            var accepted = mean >= options.MinimumMeanByte;
            var details = $"mean={mean:F1} threshold={options.MinimumMeanByte:F1}";

            if (options.LogDecisions)
                Console.WriteLine($"[BrightnessGate] {(accepted ? "ACCEPT" : "REJECT")}  {details}");

            return new FrameProcessorDecision(
                accepted,
                "brightness-gate",
                "mean-byte-threshold",
                DateTime.UtcNow,
                details);
        }
    }
}
