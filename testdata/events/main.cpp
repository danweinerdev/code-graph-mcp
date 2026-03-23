#include "widgets.h"
#include <iostream>

// C-style callback
void logEvent(const Event& event) {
    std::cout << "Event: " << event.name << std::endl;
}

// Observer class
class Logger : public IEventListener {
public:
    void onEvent(const Event& event) override {
        std::cout << "[LOG] " << event.name << " data=" << event.data << std::endl;
    }
};

int main() {
    auto& bus = EventBus::instance();

    // C-style callback
    bus.subscribeCStyle(EventType::Click, logEvent);

    // Lambda handler
    bus.subscribe(EventType::KeyPress, [](const Event& e) {
        std::cout << "Key: " << e.data << std::endl;
    });

    // Interface listener
    Logger logger;
    bus.subscribe(EventType::Click, &logger);

    // Widgets
    auto window = std::make_unique<Window>("Main Window", 800, 600);

    auto btn = std::make_unique<Button>("btn1", "Click Me");
    btn->setOnClick([](Button& b) {
        std::cout << "Button " << b.id() << " clicked!" << std::endl;
    });

    auto input = std::make_unique<TextInput>("input1");
    input->setOnChange([](const std::string& text) {
        std::cout << "Input: " << text << std::endl;
    });

    bus.subscribe(EventType::Click, btn.get());
    bus.subscribe(EventType::KeyPress, input.get());
    bus.subscribe(EventType::Resize, window.get());

    window->addChild(std::move(btn));
    window->addChild(std::move(input));

    // Simulate events
    bus.publish(Event(EventType::Click, "mouse_click"));
    bus.publish(Event(EventType::KeyPress, "key_a", 'a'));
    bus.publish(Event(EventType::Resize, "window_resize", 1024));

    std::cout << "Click listeners: " << bus.listenerCount(EventType::Click) << std::endl;

    bus.clear();
    return 0;
}
