#pragma once

#include "event.h"
#include <iostream>

class Widget : public IEventListener {
public:
    Widget(const std::string& id) : id_(id) {}
    virtual ~Widget() = default;

    const std::string& id() const { return id_; }

protected:
    std::string id_;
};

class Button : public Widget {
public:
    Button(const std::string& id, const std::string& label);

    void onEvent(const Event& event) override;
    void click();

    using ClickHandler = std::function<void(Button&)>;
    void setOnClick(ClickHandler handler) { onClick_ = handler; }

private:
    std::string label_;
    ClickHandler onClick_;
};

class TextInput : public Widget {
public:
    TextInput(const std::string& id);

    void onEvent(const Event& event) override;
    void appendChar(char c);

    const std::string& text() const { return text_; }

    using ChangeHandler = std::function<void(const std::string&)>;
    void setOnChange(ChangeHandler handler) { onChange_ = handler; }

private:
    std::string text_;
    ChangeHandler onChange_;
};

class Window : public Widget {
public:
    Window(const std::string& title, int width, int height);

    void onEvent(const Event& event) override;
    void addChild(std::unique_ptr<Widget> child);
    void resize(int width, int height);

private:
    int width_;
    int height_;
    std::vector<std::unique_ptr<Widget>> children_;
};
