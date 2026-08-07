#include "nix/store/globals.hh"
#include "nix/store/store-api.hh"
#include "nix/store/store-open.hh"
#include "nix/util/serialise.hh"

#include <nlohmann/json.hpp>

#include <iostream>
#include <stdexcept>
#include <string>

using json = nlohmann::json;

namespace {

constexpr std::size_t maximumRequestBytes = 64 * 1024;

std::string readRequest()
{
    std::string request;
    char buffer[4096];
    while (std::cin) {
        std::cin.read(buffer, sizeof(buffer));
        request.append(buffer, static_cast<std::size_t>(std::cin.gcount()));
        if (request.size() > maximumRequestBytes)
            throw std::runtime_error("export request exceeds limit");
    }
    return request;
}

int run()
{
    auto request = json::parse(readRequest());
    if (request.at("version").get<unsigned int>() != 1)
        throw std::runtime_error("unsupported export request version");

    auto store = nix::openStore(request.at("store_uri").get<std::string>());
    auto path = store->parseStorePath(request.at("path").get<std::string>());
    nix::FdSink output{nix::getStandardOutput()};
    store->narFromPath(path, output);
    output.flush();
    return 0;
}

} // namespace

int main()
{
    try {
        nix::initLibStore();
        return run();
    } catch (const std::exception & error) {
        std::cerr << error.what() << '\n';
        return 1;
    }
}
