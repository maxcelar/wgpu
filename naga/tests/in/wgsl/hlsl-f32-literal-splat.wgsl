// An `f32` literal reaches HLSL as `float`, never as a context-typed literal.
// In a ternary whose arms are both splatted literals, nothing in the context is
// `float`, so an unsuffixed `(3.1415927).xxx` is typed `double` by DXC. Under
// `-Od` that survives into the DXIL as `select double` + the double-precision
// shader flag, which a device without FP64 shader ops refuses.
struct U {
    a: vec4<f32>,
    b: vec4<f32>,
}
@group(0) @binding(0) var<uniform> u: U;

@fragment
fn main() -> @location(0) vec4<f32> {
    let phi = select(vec3(0.0), vec3(3.1415927), u.b.yzw < vec3(u.b.x));
    return vec4(cos(u.a.x * vec3(1681000.0, 1795300.0, 2208400.0) + phi), 1.0);
}
