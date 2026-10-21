struct Camera {
    view_projection: mat4x4<f32>,
    eye_time: vec4<f32>,
    floor_color: vec4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;

struct VertexIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) placement: vec4<f32>,
    @location(4) color: vec4<f32>,
    @location(5) properties: vec4<f32>,
};
struct VertexOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) color: vec4<f32>,
    @location(4) properties: vec4<f32>,
};
@vertex fn vs_main(input: VertexIn) -> VertexOut {
    var out: VertexOut;
    let world = input.position * input.placement.w + input.placement.xyz;
    out.clip = camera.view_projection * vec4<f32>(world, 1.0);
    out.world = world;
    out.normal = input.normal;
    out.uv = input.uv;
    out.color = input.color;
    out.properties = input.properties;
    return out;
}
@fragment fn fs_main(input: VertexOut) -> @location(0) vec4<f32> {
    let normal = normalize(input.normal);
    let light = normalize(vec3<f32>(-0.6, 1.0, 0.7));
    let view = normalize(camera.eye_time.xyz - input.world);
    let half_direction = normalize(view + light);
    let diffuse = max(dot(normal, light), 0.0);
    let rim = pow(1.0 - max(dot(normal, view), 0.0), 3.0);
    let specular = pow(max(dot(normal, half_direction), 0.0), 55.0) * 0.23;
    var color = input.color.rgb * (0.36 + 0.64 * diffuse) + vec3<f32>(specular);
    color += input.color.rgb * rim * 0.15;
    // A selected Cube gets a warm edge treatment, independent of its chain color.
    color += vec3<f32>(0.65, 0.30, 0.07) * rim * input.properties.x;
    if input.properties.y > 0.5 {
        let falloff = exp(-length(input.world.xz) * 0.035);
        color = camera.floor_color.rgb * (0.75 + 0.25 * falloff);
    }
    return vec4<f32>(color, 1.0);
}
@fragment fn fs_shadow(input: VertexOut) -> @location(0) vec4<f32> {
    let distance = length(input.uv * 2.0 - vec2<f32>(1.0));
    let alpha = (1.0 - smoothstep(0.0, 1.0, distance)) * 0.40;
    return vec4<f32>(0.0, 0.0, 0.0, alpha);
}
