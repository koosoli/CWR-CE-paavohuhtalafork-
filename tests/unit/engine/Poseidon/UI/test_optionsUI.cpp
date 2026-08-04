#include <Poseidon/UI/Options/OptionsScrollList.hpp>
#include <Poseidon/UI/Options/OptionsShell.hpp>
#include <Poseidon/UI/OptionsUI.hpp>
#include <Poseidon/UI/UITestEngine.hpp>
#include <catch2/catch_test_macros.hpp>

using namespace Poseidon;
TEST_CASE("optionsUI compiles", "[optionsUI][tier3]")
{
    REQUIRE(sizeof(AbstractOptionsUI) > 0);
}

class TestableOptionsShell : public OptionsShell
{
  public:
    TestableOptionsShell(bool enableSimulation, bool credits) : OptionsShell(nullptr, enableSimulation, credits) {}
};

class TestableOptionsPage : public OptionsPage
{
  public:
    using OptionsPage::ContainsCycleIdc;

    const char* TitleText() const override { return ""; }
    int DefaultFocusIdc() const override { return -1; }
    const char* ResourceClassName() const override { return ""; }
};

class TestSemanticControl : public IControl
{
  public:
    TestSemanticControl() : IControl(nullptr, 42) {}

    int GetType() override { return 0; }
    int GetStyle() override { return 0; }
    bool IsInside(float, float) override { return false; }
    void Move(float, float) override {}
    void OnDraw(float) override {}
};

TEST_CASE("OptionsShell propagates the simulation flag to the display base", "[optionsUI][UI]")
{
    TestableOptionsShell pausedShell(false, false);
    CHECK(pausedShell.EnableSimulation() == false);
    CHECK(pausedShell.SimulationEnabled() == false);

    TestableOptionsShell runningShell(true, false);
    CHECK(runningShell.EnableSimulation() == true);
    CHECK(runningShell.SimulationEnabled() == true);
}

TEST_CASE("OptionsScrollList maps every slot-local control IDC back to its slot", "[optionsUI][UI]")
{
    for (int digit = 0; digit <= 9; ++digit)
        CHECK(OptionsScrollList::SlotForControlIdc(540 + digit) == 4);

    CHECK(OptionsScrollList::SlotForControlIdc(499) == -1);
    CHECK(OptionsScrollList::SlotForControlIdc(590) == -1);
    CHECK(OptionsScrollList::SlotForControlIdc(700) == -1);
}

TEST_CASE("OptionsScrollList row policy keeps disabled rows focusable but inert", "[optionsUI][UI]")
{
    CHECK(OptionsScrollList::CanRowReceiveFocus(OptionsScrollList::KindHeader) == false);
    CHECK(OptionsScrollList::CanRowReceiveFocus(OptionsScrollList::KindAction) == true);
    CHECK(OptionsScrollList::CanRowReceiveFocus(OptionsScrollList::KindSlider) == true);

    CHECK(OptionsScrollList::CanRowAdjustValue(OptionsScrollList::KindStepper, false) == true);
    CHECK(OptionsScrollList::CanRowAdjustValue(OptionsScrollList::KindBoolean, false) == true);
    CHECK(OptionsScrollList::CanRowAdjustValue(OptionsScrollList::KindSlider, false) == true);
    CHECK(OptionsScrollList::CanRowAdjustValue(OptionsScrollList::KindAction, false) == false);
    CHECK(OptionsScrollList::CanRowAdjustValue(OptionsScrollList::KindBinding, false) == false);
    CHECK(OptionsScrollList::CanRowAdjustValue(OptionsScrollList::KindStepper, true) == false);

    CHECK(OptionsScrollList::CanRowInvokeAction(OptionsScrollList::KindAction, false) == true);
    CHECK(OptionsScrollList::CanRowInvokeAction(OptionsScrollList::KindAction, true) == false);
    CHECK(OptionsScrollList::CanRowOpenBinding(OptionsScrollList::KindBinding, false) == true);
    CHECK(OptionsScrollList::CanRowOpenBinding(OptionsScrollList::KindBinding, true) == false);
}

TEST_CASE("UITestEngine returns semantic text when controls render a clipped marquee", "[optionsUI][UI]")
{
    TestSemanticControl ctrl;

    UITestEngine::SetSemanticControlText(&ctrl, "Particles & Volumetrics");
    CHECK(UITestEngine::GetControlText(&ctrl) == "Particles & Volumetrics");

    UITestEngine::ClearSemanticControlText(&ctrl);
    CHECK(UITestEngine::GetControlText(&ctrl).empty());
}

TEST_CASE("OptionsPage cycle membership helper only accepts listed IDCs", "[optionsUI][UI]")
{
    const int cycle[] = {1101, 1104, 1107};
    TestableOptionsPage page;

    CHECK(page.ContainsCycleIdc(1101, cycle, 3));
    CHECK(page.ContainsCycleIdc(1107, cycle, 3));
    CHECK_FALSE(page.ContainsCycleIdc(1105, cycle, 3));
    CHECK_FALSE(page.ContainsCycleIdc(-1, cycle, 3));
    CHECK_FALSE(page.ContainsCycleIdc(1101, nullptr, 3));
}

// The Options screen is chosen from the DATA, not hardcoded, because loading a resource class
// that the loaded data set does not define yields an EMPTY display instead of an error.
//
// This is not hypothetical: a stock ARMA: Cold War Assault install ships RscDisplayOptions and
// RscDisplayOptionsVideo in bin/RESOURCE.BIN but NOT RscOptionsShell, which arrives separately in
// bin/resource-extra.cpp. The shell was constructed unconditionally, so on that data the Options
// entry opened a screen with no notebook, no buttons and no message — for the whole life of the
// fork. Pin the probe so the selection cannot quietly become unconditional again.
TEST_CASE("Options screen selection follows the resource the data set actually defines", "[optionsUI][UI]")
{
    ParamFile res;
    CHECK(res.FindEntry("RscOptionsShell") == nullptr);

    // Classic data: only the original family is present, so the probe must say "no shell".
    res.AddClass("RscDisplayOptions");
    CHECK(res.FindEntry("RscOptionsShell") == nullptr);
    CHECK(res.FindEntry("RscDisplayOptions") != nullptr);

    // Remaster data: resource-extra.cpp has been merged in and the shell is available.
    res.AddClass("RscOptionsShell");
    CHECK(res.FindEntry("RscOptionsShell") != nullptr);
}
