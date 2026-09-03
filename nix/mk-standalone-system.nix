# Composes a complete standalone Telchar NixOS system with caller deployment modules.
{
  nixosSystem,
  standaloneModule,
}:
{
  system,
  modules ? [ ],
  specialArgs ? { },
}:
nixosSystem {
  inherit system specialArgs;
  modules = [ standaloneModule ] ++ modules;
}
