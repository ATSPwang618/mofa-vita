"""Write the delivery identity after assets and project metadata agree."""
from .build_binding import check
from .switch import sha_file, write_json


def write_receipt(output, project, report, gate):
    assets=output/'assets';binding=check(project,assets)
    receipt=dict(schema='mofa-main-delivery-v1',validation=report,
                 manifest_sha256=binding['manifest_sha256'],project_binding=binding,
                 differential_gate_sha256=sha_file(gate),
                 install_path='ux0:data/WitchontheHolyNight/assets',
                 title_id='KNIF00540',hardware_tested=False)
    write_json(output/'delivery.json',receipt)
    instructions='''# 安装本次魔夜资源

关闭游戏，将本目录中的完整 assets 文件夹放入：

    ux0:data/WitchontheHolyNight/assets/

上级目录结构已经使用 Vita 所需的 ux0/data/WitchontheHolyNight 命名。
替换 assets 时保留 WitchontheHolyNight 下原有的存档、设置和日志。
不要将另一版 assets 的文件叠加混入本次归档。

语音和音乐分别保存在 voice.mrg 与 bgm.mrg；无需散装 stream 目录。
资源清单为 assets/vita-540p-assets.json，完整身份及检查结果见 delivery.json。

源码中的真实清单、启动 profile、图集索引与音频索引已经同步。
后续正常构建 VPK 会先检查绑定再编译这些元数据。已有旧程序不会自动更新
内部清单；需要使用按本次清单重新编译的 EBOOT/VPK。

本次验证为 PC 端文件、归档、纹理预算及音频成员核验；尚未进行 Vita 实机游戏验证。
'''
    (output/'README-安装.md').write_text(instructions,encoding='utf-8',newline='\n')
    return receipt
