#include "widgets.h"

Button::Button(const std::string& id, const std::string& label)
    : Widget(id), label_(label) {}

void Button::onEvent(const Event& event) {
    if (event.type == EventType::Click) {
        click();
    }
}

void Button::click() {
    if (onClick_) {
        onClick_(*this);
    }
}

TextInput::TextInput(const std::string& id)
    : Widget(id) {}

void TextInput::onEvent(const Event& event) {
    if (event.type == EventType::KeyPress) {
        appendChar(static_cast<char>(event.data));
    }
}

void TextInput::appendChar(char c) {
    text_ += c;
    if (onChange_) {
        onChange_(text_);
    }
}

Window::Window(const std::string& title, int width, int height)
    : Widget(title), width_(width), height_(height) {}

void Window::onEvent(const Event& event) {
    if (event.type == EventType::Resize) {
        resize(event.data, event.data);
    }
    // Forward to children.
    for (auto& child : children_) {
        child->onEvent(event);
    }
}

void Window::addChild(std::unique_ptr<Widget> child) {
    children_.push_back(std::move(child));
}

void Window::resize(int width, int height) {
    width_ = width;
    height_ = height;
}
