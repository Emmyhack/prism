# ADR-004: Timeline representation

Status: Accepted for foundation. Authoritative time is normalized rational time
with checked arithmetic. Source time, sequence time, frame rate, sample position,
and timecode become distinct types. Floats are UI/DSP boundary values only.
Interval indexes will support viewport and overlap queries; track order remains
explicit. Every inexact conversion names its rounding rule. Property tests cover
fractional rates, nested transforms, reverse/retime, and long-duration drift.
