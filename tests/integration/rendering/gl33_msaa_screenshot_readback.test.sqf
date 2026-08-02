// Guard the GL33 screenshot readback against emitting GL errors.
//
// Hosted Trident (GL33 under Xvfb) failed the I-20 GL-error gate with exactly
// one error per triScreenshot, which places the fault in the capture readback.
// dacdf77 reworked that path to bind the default read framebuffer and blit to a
// single-sample target before glReadPixels.
//
// Scope honestly: this fixture does NOT reproduce the hosted condition. The
// original diagnosis blamed a multisampled default framebuffer, but EngineGL33
// never requests multisampling on its context, and triSetMsaa does not change
// the default framebuffer's sample count either. What this does cover is that
// the reworked readback path completes cleanly and repeatedly with MSAA state
// engaged -- which is what would regress if someone simplified the blit away.
//
// Broken state: the error count is non-zero after a capture, or the capture is
// never written.

triSetLanguage "English"
triSimFrames 30

triSetMsaa 4
triSimFrames 30

// Baseline after the MSAA switch itself, so this measures the capture path
// rather than anything the mode change emitted.
triResetGLErrorBaseline
triAssertEq [(triGetGLErrorCount), 0]

triScreenshot "00_msaa_capture"
triAssertEq [(triGetGLErrorCount), 0]

// A second capture: the resolve target is created and destroyed per capture, so
// repeating it is what catches a leaked or incomplete attachment.
triScreenshot "01_msaa_capture_again"
triAssertEq [(triGetGLErrorCount), 0]

triEndTest
