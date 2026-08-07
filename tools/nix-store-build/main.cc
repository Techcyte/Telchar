#include "nix/store/build-result.hh"
#include "nix/store/derivations.hh"
#include "nix/store/globals.hh"
#include "nix/store/store-api.hh"
#include "nix/store/store-open.hh"

#include <nlohmann/json.hpp>

#include <iostream>
#include <iterator>
#include <set>
#include <string>

using json = nlohmann::json;

namespace {

constexpr std::size_t maximumRequestBytes = 16 * 1024 * 1024;

std::string readRequest()
{
    std::string request;
    request.reserve(4096);
    char buffer[4096];
    while (std::cin) {
        std::cin.read(buffer, sizeof(buffer));
        request.append(buffer, static_cast<std::size_t>(std::cin.gcount()));
        if (request.size() > maximumRequestBytes)
            throw std::runtime_error("build request exceeds limit");
    }
    return request;
}

std::string requiredString(const json & object, const char * key)
{
    if (!object.contains(key) || !object.at(key).is_string())
        throw std::runtime_error("invalid build request field");
    return object.at(key).get<std::string>();
}

nix::StorePath parseDeclaredPath(const nix::Store & store, const std::string & value)
{
    return store.parseStorePath(value);
}

int run(const std::string & storeUri, const json & request)
{
    if (request.at("version").get<unsigned int>() != 1
        || request.at("build_mode").get<unsigned int>() != 0)
        throw std::runtime_error("unsupported build request");

    auto store = nix::openStore(storeUri);
    auto drvPath = parseDeclaredPath(*store, requiredString(request, "derivation_path"));
    drvPath.requireDerivation();
    nix::BasicDerivation drv;
    drv.name = std::string(nix::BasicDerivation::nameFromPath(drvPath));
    for (const auto & source : request.at("input_sources"))
        drv.inputSrcs.insert(parseDeclaredPath(*store, source.get<std::string>()));
    drv.platform = requiredString(request, "system");
    drv.builder = requiredString(request, "builder");
    for (const auto & argument : request.at("arguments"))
        drv.args.push_back(argument.get<std::string>());
    for (const auto & entry : request.at("environment"))
        drv.env.emplace(requiredString(entry, "key"), requiredString(entry, "value"));
    for (const auto & output : request.at("outputs")) {
        auto name = requiredString(output, "name");
        auto path = parseDeclaredPath(*store, requiredString(output, "path"));
        drv.outputs.emplace(std::move(name), nix::DerivationOutput::InputAddressed{std::move(path)});
    }
    if (!std::holds_alternative<nix::DerivationType::InputAddressed>(drv.type().raw)
        || std::get<nix::DerivationType::InputAddressed>(drv.type().raw).deferred)
        throw std::runtime_error("build request is not normal input-addressed");

    auto result = store->buildDerivation(drvPath, drv, nix::bmNormal);
    auto * success = result.tryGetSuccess();
    if (success == nullptr) {
        auto * failure = result.tryGetFailure();
        if (failure == nullptr)
            throw std::runtime_error("build failed");
        throw std::runtime_error("build failed with status " + std::to_string(static_cast<unsigned int>(failure->status)));
    }
    if (success->status != nix::BuildResult::Success::Built
        && success->status != nix::BuildResult::Success::AlreadyValid)
        throw std::runtime_error("build returned unsupported success status");

    json outputs = json::array();
    std::set<std::string> declared;
    for (const auto & output : request.at("outputs")) {
        auto name = requiredString(output, "name");
        auto path = requiredString(output, "path");
        auto parsed = parseDeclaredPath(*store, path);
        if (!store->isValidPath(parsed) || !declared.insert(name).second)
            throw std::runtime_error("build output verification failed");
        outputs.push_back({name, path});
    }
    json response = {
        {"version", 1},
        {"success", true},
        {"status", success->status == nix::BuildResult::Success::Built ? "built" : "already-valid"},
        {"outputs", outputs},
    };
    std::cout << response.dump() << '\n';
    return 0;
}

} // namespace

int main(int argc, char ** argv)
{
    try {
        if (argc != 2 || argv[1][0] == '\0')
            throw std::runtime_error("expected one fixed store URI");
        nix::initLibStore();
        auto request = json::parse(readRequest());
        return run(argv[1], request);
    } catch (const std::exception & error) {
        std::cerr << "build helper failed: " << error.what() << '\n';
        return 1;
    }
}
