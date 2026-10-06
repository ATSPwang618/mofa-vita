varying vec2 v_uv;
uniform vec4 u_color, u_operation;
uniform sampler2D u_mask, u_previous;
uniform vec2 u_mask_size, u_previous_size;
uniform float u_kind;
uniform float u_channel;
void main() {
    vec4 color = u_channel != 0.0 ? texture2D(u_source, v_uv) : linear_source(v_uv * u_extent.xy);
    if (u_operation.y != 0.0) color = vec4(u_color.rgb, color.a * u_color.a);
    else color *= u_color;
    color.a = clamp(color.a * u_operation.x, 0.0, 1.0);
    if (u_operation.w != 0.0) color.a = floor(color.a * 255.0 * (255.0/256.0))/255.0;
    if (u_operation.z != 0.0 && texture2D(u_mask, gl_FragCoord.xy/u_mask_size).a < (128.0/255.0)) discard;
    // ES2 devices lacking EXT_blend_minmax retain the previous alpha with a
    // separate GPU-only alpha pass. RGB is already blended with source alpha.
    if (u_kind != 0.0) color.a = max(color.a, texture2D(u_previous, gl_FragCoord.xy/u_previous_size).a);
    gl_FragColor = color;
}
