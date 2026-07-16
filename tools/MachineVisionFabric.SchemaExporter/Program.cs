using System.Text.Json;
using System.Text.Json.Nodes;
using System.Text.Json.Schema;
using System.Text.Json.Serialization.Metadata;
using MachineVisionFabric.Contracts.Integrations;
using MachineVisionFabric.Contracts.Packages;
using MachineVisionFabric.Contracts.Control;
using MachineVisionFabric.Contracts.Simulation;

var outputRoot = args.Length > 0
    ? Path.GetFullPath(args[0])
    : Path.Combine(AppContext.BaseDirectory, "schemas");

Directory.CreateDirectory(outputRoot);

var jsonOptions = new JsonSerializerOptions(JsonSerializerDefaults.Web)
{
    WriteIndented = true,
    TypeInfoResolver = new DefaultJsonTypeInfoResolver()
};

WriteSchema<FabricProfileManifest>("fabric-profile-manifest.schema.json");
WriteSchema<FabricRuntimeProfile>("fabric-runtime-profile.schema.json");
WriteSchema<SourceBinding>("source-binding.schema.json");
WriteSchema<IntegrationModuleDescriptor>("integration-module-descriptor.schema.json");
WriteSchema<IntegrationModuleManifest>("integration-module-manifest.schema.json");
WriteSchema<IntegrationCapabilityDescriptor>("integration-capability-descriptor.schema.json");
WriteSchema<ProductPresenceGateBinding>("product-presence-gate-binding.schema.json");
WriteSchema<S7GatewayGateOptions>("s7-gateway-gate-options.schema.json");
WriteSchema<S7SignalAddress>("s7-signal-address.schema.json");
WriteSchema<SimulatedPlcGateOptions>("simulated-plc-gate-options.schema.json");
WriteSchema<TcpSignalGateOptions>("tcp-signal-gate-options.schema.json");
WriteSchema<FolderSequenceSourceOptions>("folder-sequence-source-options.schema.json");

return;

void WriteSchema<T>(string fileName)
{
    JsonNode schema = JsonSchemaExporter.GetJsonSchemaAsNode(jsonOptions, typeof(T), exporterOptions: null);
    File.WriteAllText(Path.Combine(outputRoot, fileName), schema.ToJsonString(jsonOptions));
}
