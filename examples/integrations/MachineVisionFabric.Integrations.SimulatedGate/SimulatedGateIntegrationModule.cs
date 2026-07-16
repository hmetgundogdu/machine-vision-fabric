using MachineVisionFabric.Contracts.Control;
using MachineVisionFabric.Core.Abstractions;
using MachineVisionFabric.Sdk;

namespace MachineVisionFabric.Integrations.SimulatedGate;

public sealed class SimulatedGateIntegrationModule : ProductPresenceGateModuleBase<SimulatedPlcGateOptions>
{
    protected override MachineVisionFabric.Contracts.Integrations.IntegrationModuleDescriptor BuildDescriptor()
    {
        return IntegrationModuleDescriptorBuilder.CreateGate<SimulatedPlcGateOptions>(
            "mvf.simulated-gate",
            "Simulated Product Presence Gate",
            "0.1.0",
            "product-presence-gate",
            "Simulated PLC-backed product presence gate for dataset collection tests.");
    }

    protected override IProductPresenceGate CreateGate(SimulatedPlcGateOptions options)
    {
        return new SimulatedModuleGate(options);
    }

    private sealed class SimulatedModuleGate(SimulatedPlcGateOptions options) : IProductPresenceGate
    {
        private int evaluationCount;
        private readonly DateTime startedAtUtc = DateTime.UtcNow;

        public Task<ProductPresenceDecision> EvaluateAsync(CancellationToken cancellationToken)
        {
            cancellationToken.ThrowIfCancellationRequested();

            var productPresent = ResolveProductPresent();

            return Task.FromResult(new ProductPresenceDecision(
                options.Enabled && productPresent,
                options.SourceName,
                options.StationId,
                DateTime.UtcNow,
                options.Details));
        }

        private bool ResolveProductPresent()
        {
            if (options.DelayBeforePresentMs > 0)
            {
                var elapsed = DateTime.UtcNow - startedAtUtc;
                if (elapsed < TimeSpan.FromMilliseconds(options.DelayBeforePresentMs))
                {
                    return false;
                }

                return true;
            }

            if (options.ProductPresentSequence.Count == 0)
            {
                return options.ProductPresent;
            }

            var currentIndex = Interlocked.Increment(ref evaluationCount) - 1;
            if (currentIndex < options.ProductPresentSequence.Count)
            {
                return options.ProductPresentSequence[currentIndex];
            }

            return options.HoldLastSequenceValue
                ? options.ProductPresentSequence[^1]
                : options.ProductPresent;
        }
    }
}
