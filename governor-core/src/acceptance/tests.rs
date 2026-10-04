//! F24 tests, split by the module's seams: the marked-file reading, the
//! freeze record and assessment binding, and the judgment/repair deadlines.
//! Every input is a constructed value — bytes, digests, times — never a live
//! system.

mod builders;
mod f24_binding;
mod f24_deadlines;
mod f24_reading;
mod f24_verdict;
