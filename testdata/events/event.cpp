#include "event.h"

void EventBus::subscribe(EventType type, EventHandler handler) {
    handlers_[static_cast<int>(type)].push_back(Subscription{handler});
}

void EventBus::subscribe(EventType type, IEventListener* listener) {
    listeners_[static_cast<int>(type)].push_back(listener);
}

void EventBus::subscribeCStyle(EventType type, EventCallback callback) {
    cCallbacks_[static_cast<int>(type)].push_back(callback);
}

void EventBus::publish(const Event& event) {
    int key = static_cast<int>(event.type);

    // Call std::function handlers.
    auto it = handlers_.find(key);
    if (it != handlers_.end()) {
        for (auto& sub : it->second) {
            sub.handler(event);
        }
    }

    // Call interface listeners.
    auto lit = listeners_.find(key);
    if (lit != listeners_.end()) {
        for (auto* listener : lit->second) {
            listener->onEvent(event);
        }
    }

    // Call C-style callbacks.
    auto cit = cCallbacks_.find(key);
    if (cit != cCallbacks_.end()) {
        for (auto cb : cit->second) {
            cb(event);
        }
    }
}

void EventBus::clear() {
    handlers_.clear();
    listeners_.clear();
    cCallbacks_.clear();
}

size_t EventBus::listenerCount(EventType type) const {
    size_t count = 0;
    int key = static_cast<int>(type);

    auto it = handlers_.find(key);
    if (it != handlers_.end()) count += it->second.size();

    auto lit = listeners_.find(key);
    if (lit != listeners_.end()) count += lit->second.size();

    auto cit = cCallbacks_.find(key);
    if (cit != cCallbacks_.end()) count += cit->second.size();

    return count;
}
