#include "pipeline.h"
#include "result.h"
#include "concepts.h"
#include <iostream>
#include <string>

// Auto return types
auto makeGreeting(const std::string& name) -> std::string {
    return "Hello, " + name + "!";
}

auto square(int x) {
    return x * x;
}

// Using Result type
Result<int> safeDivide(int a, int b) {
    if (b == 0) return Result<int>::err("division by zero");
    return Result<int>::ok(a / b);
}

Result<std::string> readConfig(const std::string& path) {
    if (path.empty()) return Result<std::string>::err("empty path");
    return Result<std::string>::ok("config data from " + path);
}

void testPipeline() {
    auto numbers = Pipeline<int>({5, 3, 8, 1, 9, 2, 7, 4, 6});

    auto result = numbers
        .filter([](const int& n) { return n > 3; })
        .sort()
        .transform([](int n) { return n * 2; });

    result.forEach([](const int& n) {
        std::cout << n << " ";
    });
    std::cout << std::endl;

    int sum = numbers.reduce(0, [](int acc, int n) { return acc + n; });
    std::cout << "Sum: " << sum << std::endl;
}

void testResult() {
    auto r1 = safeDivide(10, 3);
    if (r1.isOk()) {
        std::cout << "10/3 = " << r1.value() << std::endl;
    }

    auto r2 = safeDivide(10, 0);
    if (r2.isErr()) {
        std::cout << "Error: " << r2.error() << std::endl;
    }

    auto r3 = readConfig("/etc/app.conf");
    auto mapped = r3.map([](const std::string& s) { return s.length(); });
    std::cout << "Config length: " << mapped.valueOr(0) << std::endl;
}

void testModern() {
    // constexpr
    constexpr int fact5 = factorial(5);
    std::cout << "5! = " << fact5 << std::endl;

    // Scoped enums
    auto settings = config::defaults::Settings::createDefault();
    if (settings.logLevel == LogLevel::Info) {
        std::cout << "Log level: Info" << std::endl;
    }

    // RAII handle
    UniqueHandle<std::string> handle(new std::string("managed resource"));
    std::cout << "Handle: " << *handle << std::endl;

    // Using aliases
    StringVec names = {"Alice", "Bob", "Charlie"};
    auto namePipeline = Pipeline<std::string>(names);
    auto lengths = namePipeline.transform([](const std::string& s) {
        return static_cast<int>(s.length());
    });
    std::cout << "Name count: " << lengths.count() << std::endl;
}

int main() {
    std::cout << makeGreeting("World") << std::endl;
    std::cout << "4^2 = " << square(4) << std::endl;

    testPipeline();
    testResult();
    testModern();

    return 0;
}
