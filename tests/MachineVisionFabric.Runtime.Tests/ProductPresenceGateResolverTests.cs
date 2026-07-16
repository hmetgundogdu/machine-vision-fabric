using System.Text.Json;
using MachineVisionFabric.Contracts.Control;
using MachineVisionFabric.Contracts.Integrations;
using MachineVisionFabric.Contracts.Packages;
using MachineVisionFabric.Core.Abstractions;
using MachineVisionFabric.Runtime;
using Microsoft.Extensions.Logging.Abstractions;
using Microsoft.Extensions.Options;

namespace MachineVisionFabric.Runtime.Tests;

public sealed class ProductPresenceGateResolverTests
{
    [Fact]
    public async Task Resolve_UsesExternalModule_WhenProfileRequestsModule()
    {
        var runtimeOptions = Options.Create(new MachineVisionFabricRuntimeOptions());
        var loader = new FakeLoader(
            new FakeProductPresenceGateModule("mvf.fake-module", productPresent: false));
        var resolver = new ProfileProductPresenceGateResolver(runtimeOptions, loader, NullLogger<ProfileProductPresenceGateResolver>.Instance);

        var manifest = new FabricProfileManifest
        {
            ProductPresenceGate = new ProductPresenceGateBinding
            {
                Mode = "module",
                ModuleId = "mvf.fake-module",
                Config = JsonSerializer.SerializeToElement(new SimulatedPlcGateOptions
                {
                    Enabled = true,
                    ProductPresent = false,
                    SourceName = "fake-module"
                })
            }
        };

        var resolution = resolver.Resolve(manifest, integrationsRoot: ".");
        var decision = await resolution.Gate.EvaluateAsync(CancellationToken.None);

        Assert.Equal("module", resolution.Strategy);
        Assert.Equal("mvf.fake-module", resolution.Source);
        Assert.False(decision.ProductPresent);
    }

    [Fact]
    public async Task Resolve_UsesModuleConfigurationLoadedFromCamelCaseJson()
    {
        var runtimeOptions = Options.Create(new MachineVisionFabricRuntimeOptions());
        var loader = new FakeLoader(new MachineVisionFabric.Integrations.SimulatedGate.SimulatedGateIntegrationModule());
        var resolver = new ProfileProductPresenceGateResolver(runtimeOptions, loader, NullLogger<ProfileProductPresenceGateResolver>.Instance);

        var manifest = new FabricProfileManifest
        {
            ProductPresenceGate = new ProductPresenceGateBinding
            {
                Mode = "module",
                ModuleId = "mvf.simulated-gate",
                Config = JsonDocument.Parse(
                    """
                    {
                      "enabled": true,
                      "productPresent": false,
                      "sourceName": "manifest-module",
                      "stationId": "station-2"
                    }
                    """).RootElement
            }
        };

        var resolution = resolver.Resolve(manifest, integrationsRoot: ".");
        var decision = await resolution.Gate.EvaluateAsync(CancellationToken.None);

        Assert.Equal("module", resolution.Strategy);
        Assert.Equal("mvf.simulated-gate", resolution.Source);
        Assert.False(decision.ProductPresent);
        Assert.Equal("manifest-module", decision.Source);
        Assert.Equal("station-2", decision.StationId);
    }

    private sealed class FakeLoader(params IIntegrationModule[] modules) : IIntegrationModuleLoader
    {
        public IReadOnlyList<IIntegrationModule> LoadModules(string pluginRoot) => modules;
    }

    private sealed class FakeProductPresenceGateModule(string moduleId, bool productPresent) : IProductPresenceGateModule
    {
        public IntegrationModuleDescriptor Describe()
        {
            return new IntegrationModuleDescriptor
            {
                ModuleId = moduleId,
                DisplayName = "Fake Gate",
                Version = "0.1.0",
                Capabilities =
                [
                    new IntegrationCapabilityDescriptor
                    {
                        Name = "fake-gate",
                        Kind = IntegrationCapabilityKind.Gate,
                        SchemaType = typeof(SimulatedPlcGateOptions).FullName ?? nameof(SimulatedPlcGateOptions)
                    }
                ]
            };
        }

        public IProductPresenceGate CreateGate(JsonElement configuration)
        {
            return new FakeGate(productPresent);
        }

        private sealed class FakeGate(bool productPresent) : IProductPresenceGate
        {
            public Task<ProductPresenceDecision> EvaluateAsync(CancellationToken cancellationToken)
            {
                return Task.FromResult(new ProductPresenceDecision(
                    productPresent,
                    "fake-gate",
                    "station-1",
                    DateTime.UtcNow));
            }
        }
    }
}
