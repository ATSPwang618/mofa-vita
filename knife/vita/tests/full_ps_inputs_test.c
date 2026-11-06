/* Full-material OP1/6/13 exercise the production public C input validator.
 * Literal positive/negative fixtures protect the generic PS source contract;
 * the separate plain tests keep IDs21/30/31 constrained. No GPU execution. */
#ifndef KV_GXM_BACKEND_SOURCE
#define KV_GXM_BACKEND_SOURCE "../gxm_backend.c"
#endif
#include KV_GXM_BACKEND_SOURCE

static unsigned checks, failures;
static void expect(int value, const char *expression, int line) {
    ++checks;
    if (!value) { ++failures; fprintf(stderr, "FAIL line %d: %s\n", line, expression); }
}
#define EXPECT(value) expect(!!(value), #value, __LINE__)
typedef struct Input { KvEffectVertex v[3]; float p[8]; } Input;
static Input fresh(float operation) {
    Input in = { .p = {operation,0,960,540,-1,0,0,0} };
    for (unsigned v=0;v<3;++v) in.v[v].color[3]=1;
    return in;
}
static void check(Input in, uint32_t shader, uint32_t blend, int expected) {
    Input before=in;
    EXPECT(effect_inputs(in.v,3,shader,blend,in.p)==expected);
    EXPECT(!memcmp(&in,&before,sizeof(in)));
}
static void full(uint32_t shader, float operation) {
    Input base=fresh(operation);
    check(base,shader,5,0);
    for(unsigned fill=0;fill<2;++fill) for(unsigned noalpha=0;noalpha<2;++noalpha) {
        Input in=base; in.p[1]=fill; in.p[6]=noalpha; check(in,shader,5,0);
    }
    /* Each haze/soft member is retained, not merely a common zero fixture.
       Shared haze and varying soft values match the existing ID29 contract. */
    for(unsigned field=8;field<28;++field) {
        Input in=base;
        for(unsigned v=0;v<3;++v) {
            float values[28]; memcpy(values,&in.v[v],sizeof(values));
            values[field]=.25f;
            if(field==24) values[26]=4;
            memcpy(&in.v[v],values,sizeof(values));
        }
        check(in,shader,5,0);
    }
    Input in=base; in.v[1].soft[0][0]=.5f; in.v[2].soft[1][1]=.75f;
    in.v[2].soft[2][0]=1; in.v[2].soft[2][1]=17; in.v[2].soft[2][2]=256;
    check(in,shader,5,0);
    for(unsigned flags=8;flags<=2047;flags+=2039) {
        in=base; in.p[4]=.5f; in.p[5]=(float)flags; check(in,shader,5,0);
    }
    in=base; in.p[7]=1; check(in,shader,5,0);
    in.p[1]=1; check(in,shader,5,0); /* Fill with group mask is valid. */
    for(unsigned op=0;op<=25;++op) {
        in=base; in.p[0]=(float)op;
        check(in,shader,5,op==operation ? 0 : KV_BAD_ARGUMENT);
    }
    in=base; in.p[0]=operation+.5f; check(in,shader,5,KV_BAD_ARGUMENT);
    for(unsigned b=0;b<10;++b) check(base,shader,b,b==5 ? 0 : KV_BAD_ARGUMENT);
    for(unsigned field=0;field<8;++field) {
        in=base; in.p[field]=INFINITY; check(in,shader,5,KV_BAD_ARGUMENT);
        in=base; in.p[field]=NAN; check(in,shader,5,KV_BAD_ARGUMENT);
    }
    for(unsigned v=0;v<3;++v) for(unsigned field=0;field<28;++field) {
        in=base; float values[28]; memcpy(values,&in.v[v],sizeof(values));
        values[field]=INFINITY; memcpy(&in.v[v],values,sizeof(values));
        check(in,shader,5,KV_BAD_ARGUMENT);
    }
    for(unsigned v=1;v<3;++v) for(unsigned row=0;row<2;++row) for(unsigned c=0;c<4;++c) {
        in=base; in.v[v].haze[row][c]=.5f; check(in,shader,5,KV_BAD_ARGUMENT);
    }
    const float bad_flags[]={7,2048,8.5f};
    for(unsigned i=0;i<3;++i) {
        in=base; in.p[4]=0; in.p[5]=bad_flags[i]; check(in,shader,5,KV_BAD_ARGUMENT);
    }
    in=base; in.p[4]=-0.0f; in.p[5]=0; check(in,shader,5,KV_BAD_ARGUMENT);
    const float bad_group[]={-1,.5f,2};
    for(unsigned i=0;i<3;++i) { in=base; in.p[7]=bad_group[i]; check(in,shader,5,KV_BAD_ARGUMENT); }
    in=base; in.p[7]=1; in.p[4]=0; in.p[5]=8; check(in,shader,5,KV_BAD_ARGUMENT);
    in=base; in.p[7]=1; in.p[6]=1; check(in,shader,5,KV_BAD_ARGUMENT);
    in=base; in.p[7]=1; in.p[6]=-.0001f; check(in,shader,5,KV_BAD_ARGUMENT);
    for(unsigned field=2;field<=3;++field) {
        in=base; in.p[field]=0; check(in,shader,5,KV_BAD_ARGUMENT);
        in.p[field]=-1; check(in,shader,5,KV_BAD_ARGUMENT);
    }
    for(unsigned v=0;v<3;++v) {
        in=base; in.v[v].soft[2][0]=-1; check(in,shader,5,KV_BAD_ARGUMENT);
        in.v[v].soft[2][0]=1; in.v[v].soft[2][1]=18; in.v[v].soft[2][2]=4;
        check(in,shader,5,KV_BAD_ARGUMENT);
        in.v[v].soft[2][1]=0; in.v[v].soft[2][2]=257; check(in,shader,5,KV_BAD_ARGUMENT);
    }
}
int main(void) {
    _Static_assert(sizeof(KvEffectVertex)==112,"complete vertex ABI");
    full(54,1); full(55,6); full(56,13);
    for(unsigned op=1;op<=24;++op) full(63+op,(float)op);
    Input base=fresh(1);
    for(unsigned id=57;id<=63;++id) check(base,id,5,KV_BAD_ARGUMENT);
    check(base,88,5,KV_BAD_ARGUMENT); check(base,127,5,KV_BAD_ARGUMENT);
    check(base,UINT32_MAX,5,KV_BAD_ARGUMENT);
    printf("RESULT %s: %u checks, %u failures; full PS source inputs, no GPU execution\n",
        failures ? "FAIL" : "PASS",checks,failures);
    return failures ? 1 : 0;
}
