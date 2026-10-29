
use super::*;
use crate::redist::TestMode;

#[test]
fn a_cuda_wheel_is_python_prefixed_unless_the_mode_is_on() {
    {
        let _off = TestMode::set(false);
        assert_eq!(rpm_name("nvidia-cublas"), "python-nvidia-cublas");
        assert_eq!(
            rpm_name("nvidia-cuda-runtime"),
            "python-nvidia-cuda-runtime"
        );
        assert_eq!(rpm_name("cuda-cublas"), "python-cuda-cublas");
    }
    let _on = TestMode::set(true);
    assert_eq!(rpm_name("nvidia-cublas"), "cuda-cublas");
    assert_eq!(rpm_name("nvidia_cublas_cu13"), "cuda-cublas");
    assert_eq!(rpm_name("nvidia-cuda-runtime"), "cuda-cudart");
    assert_eq!(rpm_name("nvidia-cudnn-cu13"), "cuda-cudnn");
    // Idempotent for the package's own name, and untouched for the rest.
    assert_eq!(rpm_name("cuda-cublas"), "cuda-cublas");
    assert_eq!(rpm_name("numpy"), "python-numpy");
    assert_eq!(rpm_name("nvidia-smi"), "python-nvidia-smi");
    // The node and the package it becomes agree on the comparison key.
    assert_eq!(
        fuzzy_key(&rpm_name("nvidia-cublas")),
        fuzzy_key("cuda-cublas")
    );
}
