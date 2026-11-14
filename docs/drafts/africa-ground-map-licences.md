# Licences of the Africa ground-map inputs (study, order 1742)

Refs #232. This is a study only. It has no product code and no periculum
cell. Stage A of the Sahara ground map (ground classes over the
desert) draws on remote-sensing inputs whose licences
the pilot did not check. Lew's rule is that we use only sources we may
redistribute, and that OpenStreetMap stays the only source for anything
OSM maps. This page checks each input against the licence its publisher
states on its own page, so that 1743 can cut and ship packs from them
and name them correctly.

All pages were read on **2026-11-14** (UTC). Text in quotation marks is
copied from the page named in the same row. The exceptions are the three
usgs.gov pages. They answered 403 to curl and could only be read through
a summarising fetcher. Their quotes are short phrases the fetcher gave
back. They are marked "(via fetcher)", and someone must check them in a
browser before 1743 commits a credit line. Every Landsat licence sentence
that 1743 relies on comes from the two copies Digital Earth Africa (DE
Africa) publishes itself, both quoted below in full.

## Short answer

| Input | Licence | Redistribute derived tiles? | Share-alike | Non-commercial | Verdict |
|---|---|---|---|---|---|
| WOfS (`wofs_ls`, `wofs_ls_summary_annual`, `wofs_ls_summary_alltime`) | CC BY 4.0 | yes, with credit and a note of modification | no | no | use |
| Sentinel-1 RTC (`s1_rtc`) | Copernicus Sentinel data terms (EU Delegated Regulation 1159/2013) | yes, with the "Contains modified Copernicus Sentinel data [Year]" notice | no | no | use |
| ALOS/ALOS-2 PALSAR annual mosaic (`alos_palsar_mosaic`) | JAXA "Terms of Use of Research Data" | yes, with credit to JAXA | no | no, but commercial use needs **advance notice to JAXA** | use, with the caveat for Lew below |
| Landsat C2 L2 surface temperature (`ls5_st`, `ls7_st`, `ls8_st`, `ls9_st`) | USGS, no restrictions (US federal data) | yes | no | no | use |
| GeoMAD (`gm_s2_annual`, `gm_ls8_annual`, ...), checked earlier | CC BY 4.0 | yes, with credit | no | no | use |
| Kellndorfer et al. global S1 coherence/backscatter, checked earlier | CC BY 4.0, plus the Copernicus notice | yes, with credit | no | no | use |

None of the six has a share-alike clause or a non-commercial clause, and
all six allow derived data to be redistributed. **No input has to be
replaced.** The one clause that goes beyond attribution is JAXA's
advance notice for commercial use (section 3). It does not stop us from
redistributing. Whether it is acceptable is Lew's call, and the fallback
if it is not is given there.

## 1. Water Observations from Space (WOfS), DE Africa

- Page: https://docs.digitalearthafrica.org/en/latest/data_specs/Landsat_WOfS_specs.html,
  section "License": "CC BY Attribution 4.0 International License".
- Same dataset in the AWS Open Data registry
  (https://raw.githubusercontent.com/awslabs/open-data-registry/main/datasets/deafrica-wofs.yaml,
  `License:`): "DE Africa makes this data available under the Creative
  Commons Attribute 4.0 license https://creativecommons.org/licenses/by/4.0/."
- Acknowledgment on the page. It is requested, not a licence condition:
  "The WOfS algorithms incorporated in this product are the work of
  Norman Mueller, Geoscience Australia, and Dr Dale Roberts, Australian
  National University."
- Reference the page gives: Mueller, N. et al. (2016), "Water
  observations from space: Mapping surface water from 25 years of
  Landsat imagery across Australia", Remote Sensing of Environment 174,
  341-352, https://doi.org/10.1016/j.rse.2015.11.003.
- Redistributing derived data: allowed. CC BY 4.0 (legal code
  https://creativecommons.org/licenses/by/4.0/legalcode.txt) Section 3(a)(1)
  requires us, when we share the material "including in modified form",
  to keep the creator identification, a copyright notice, a licence
  notice, a notice of the warranty disclaimer and a URI where practicable
  (a), to "indicate if You modified the Licensed Material" (b), and to
  name the licence with its URI (c). Section 3(a)(2) allows all of this
  "in any reasonable manner based on the medium". A credit screen plus
  the pack's directory piece is reasonable, and that is how the
  Copernicus DEM packs already do it (THIRD-PARTY.md, `erg-chebbi.lmh`
  row).
- Share-alike: none. Non-commercial: none. Section 2(a)(5)(B) "No
  downstream restrictions" forbids terms or "Effective Technological
  Measures" that restrict recipients. Our packs are plain open formats,
  so this costs nothing, but a future encrypted or DRM pack format would
  break it.
- Credit line for the ground map: **"Water Observations from Space ©
  Digital Earth Africa, CC BY 4.0, modified (resampled and classified)"**,
  plus the Landsat source credit (section 4), because WOfS is derived
  from USGS Landsat.

## 2. Sentinel-1 Radiometric Terrain Corrected backscatter (`s1_rtc`), DE Africa

- Page: https://docs.digitalearthafrica.org/en/latest/data_specs/Sentinel-1_specs.html,
  section "License": "Access to Sentinel data is free, full and open for
  the broad Regional, National, European and International user
  community. View Terms and Conditions." The link on the page goes to
  `scihub.copernicus.eu/twiki/...`, which did not answer (curl status 000;
  SciHub was shut down in 2023). The AWS registry entry for the same
  bucket
  (https://raw.githubusercontent.com/awslabs/open-data-registry/main/datasets/deafrica-sentinel-1.yaml)
  links the governing text instead:
  https://sentinels.copernicus.eu/documents/247904/690755/Sentinel_Data_Legal_Notice
  (2-page PDF, sha256
  `fa2955ff48a1d82e77fc7296d63681670ecdb9d2811a0505ae60d0683b62fa64`).
- Provenance on the page: "DE Africa Sentinel-1 dataset is processed by
  Sinergise Sentinel Hub using ESA Sentinel-1 GRD as input. Radiometric
  terrain correction is applied using Copernicus DEM". DE Africa's
  helpdesk article on conditions of use
  (https://helpdesk.digitalearthafrica.org/portal/en/kb/articles/waht)
  sends analysis-ready data (Sentinel-1, Sentinel-2, Landsat) to "the
  license sections in the dataset description page". So the Copernicus
  terms govern, and neither DE Africa nor Sinergise adds a licence of its
  own.
- The legal notice, quoted: "EU law grants free access to Copernicus
  Sentinel Data and Service Information for the purpose of the following
  use in so far as it is lawful: (a) reproduction; (b) distribution;
  (c) communication to the public; (d) adaptation, modification and
  combination with other data and information; (e) any combination of
  points (a) to (d)." And: "Where the Copernicus Sentinel Data and
  Service Information have been adapted or modified, the user shall
  provide the following notice: (1) 'Contains modified Copernicus
  Sentinel data [Year]' for Sentinel data". The notice names Regulation
  (EU) No 377/2014 and Delegated Regulation (EU) No 1159/2013 as its
  legal basis.
- Redistributing derived data: allowed. Share-alike: none.
  Non-commercial: none. The notice names "specific limitations of access
  and use in the rare cases of security concerns, protection of third
  party rights or risk of service disruption". Those limit access, not
  data we already hold.
- Credit line: **"Contains modified Copernicus Sentinel data [years of
  the scenes used], processed by Digital Earth Africa (Sentinel-1 RTC)"**.
  The Copernicus half is mandatory. The DE Africa half is courtesy,
  since the page asks for none.
- The Copernicus DEM used inside the terrain correction is not
  redistributed as heights. It is already credited where we do ship it
  (the height packs).

## 3. ALOS/ALOS-2 PALSAR annual mosaic (`alos_palsar_mosaic`), JAXA via DE Africa

- DE Africa page:
  https://docs.digitalearthafrica.org/en/latest/data_specs/ALOS_PALSAR_annual_mosaic_specs.html,
  "License": "This dataset is available to use with no charge under the
  Terms for Use." "Acknowledgements": "This dataset is part of a global
  product provided by the Japan Aerospace Exploration Agency (JAXA)."
  The registry entry (`deafrica-alos-jers.yaml`) says the same:
  "Data is available for free under the terms of use".
- JAXA's product page (https://www.eorc.jaxa.jp/ALOS/en/dataset/fnf_e.htm):
  "The global 25m resolution SAR mosaics and forest/non-forest maps are
  free and open annual datasets generated by JAXA and GAMMA Remote
  Sensing ..." and "This dataset is available to use with no charge under
  the "Terms of Use of Research Data"."
- The terms themselves. `https://earth.jaxa.jp/policy/en.html` redirects
  to https://earth.jaxa.jp/en/data/policy/, "Terms of Use of Research
  Data", Space Technology Directorate I:
  - "As long as complying with the following paragraphs 2.1 – 2.5, the
    user can use, modify or distribute to the third party the Research
    Data at his/her disposal without a fee."
  - 2.1 Credit: "The user also needs to credit JAXA as the original
    distributor when distributing any derivatives of the Research Data to
    any third party or making such derivatives public." Example credit
    lines include "Credit: XXXX (JAXA)" and "The original data used for
    this product have been supplied by JAXA's XXXX."
  - 2.2: notifying JAXA when a product is publicised "would be
    appreciated". This is a request, not a condition.
  - **2.3 Commercial purposes use: "The user can use the Research Data
    for commercial purposes without a fee; however, the user needs to
    notify JAXA thereof in advance."**
  - 2.5: "The terms are subject to change without notice."
  - 3: "The copyrights of the products provided in the Service are the
    property of JAXA."
  - The page also requires users to follow JAXA's site policy at
    https://global.jaxa.jp/policy.html. That page was not read in this
    pass (see Open in the report).
- Redistributing derived data: allowed, with credit. Share-alike: none.
  Non-commercial: none. Commercial use is allowed but needs advance
  notice to JAXA.
- What the notice clause means for us. Leviculum distributes the packs
  free of charge. Anyone who redistributes them commercially (for
  example a shop that sells boards with maps preloaded) would have to
  tell JAXA first. That is a condition on recipients, not a ban, so the
  data is redistributable under Lew's rule. It is still less free than
  CC BY, the terms can change without notice, and the data is not under
  a standard licence. Two ways forward:
  1. **Keep it (recommended).** Ship the credit line and state the
     notification duty next to it in the pack's directory piece and in
     THIRD-PARTY.md, so a commercial redistributor learns about it.
  2. **Drop it.** Whatever class the pilot fed from L-band falls back
     to C-band only (this pass did not find the pilot's class recipe,
     see Open in the report): `s1_rtc` and the Kellndorfer coherence and backscatter.
     That class may separate less well, but nothing else is lost. There is
     no other open global L-band annual mosaic for Africa, so no
     replacement source can be named.
- Credit line: **"Credit: ALOS/ALOS-2 PALSAR/PALSAR-2 annual mosaic
  (JAXA), via Digital Earth Africa; modified"**.

## 4. Landsat Collection 2 Level-2 surface temperature (`ls5_st` .. `ls9_st`), USGS via DE Africa

- DE Africa page:
  https://docs.digitalearthafrica.org/en/latest/data_specs/Landsat_C2_ST_specs.html,
  "License": "There are no restrictions on Landsat data downloaded from
  the USGS; it can be used or redistributed as desired. USGS request that
  you include a statement of the data source when citing, copying, or
  reprinting USGS Landsat data or images." "Acknowledgements": "Landsat
  Level- 2 Surface Temperature Science Product courtesy of the U.S.
  Geological Survey." The registry entry (`deafrica-landsat.yaml`) gives
  the same sentence. DE Africa hosts the USGS product unchanged (bucket
  `deafrica-landsat`, path `collection02/level-2/standard/...`).
- USGS's own pages (via fetcher, 403 to curl):
  https://www.usgs.gov/landsat-missions/landsat-collection-2-level-2-science-products
  says "There are no restrictions on the use of Landsat products." It
  suggests the credit "Landsat Collection 2 Level-2 image courtesy of the
  U.S. Geological Survey" and gives the dataset DOIs: Landsat 8-9
  OLI/TIRS L2 C2 https://doi.org/10.5066/P9OGBGM6, Landsat 7 ETM+ L2 C2
  https://doi.org/10.5066/P9C7I13B, Landsat 4-5 TM L2 C2
  https://doi.org/10.5066/P9IAXOVV. https://www.usgs.gov/centers/eros/data-citation
  says the EROS products are "federally created data and therefore
  reside in the public domain".
- Redistributing derived data: allowed. The data is public domain
  (US federal). Share-alike: none. Non-commercial: none.
- Credit line: **"Landsat Collection 2 Level-2 surface temperature
  courtesy of the U.S. Geological Survey"**. It is requested, not a
  legal condition, and we give it anyway.

## 5. GeoMAD, DE Africa (checked earlier, re-read here)

- Page: https://docs.digitalearthafrica.org/en/latest/data_specs/GeoMAD_specs.html,
  "License": "CC BY Attribution 4.0 International License".
  "Acknowledgements": "The high-dimensional statistics algorithms
  incorporated in this service are the work of Dr Dale Roberts,
  Australian National University." Products: `gm_s2_annual`,
  `gm_ls8_annual`, `gm_ls5_ls7_annual`, `gm_s2_semiannual`,
  `gm_s2_rolling`.
- The helpdesk article (section 2) says: "The services of Digital Earth
  Africa are distributed under the license" CC BY 4.0.
- Same terms as WOfS (section 1). A `gm_s2_*` product is built from
  Sentinel-2. We add the Copernicus notice anyway, because the legal
  notice in section 2 binds whoever distributes adapted Sentinel data.
- Credit line: **"GeoMAD © Digital Earth Africa, CC BY 4.0, modified;
  contains modified Copernicus Sentinel data [years]"** for `gm_s2_*`,
  or with the Landsat credit for `gm_ls*`.

## 6. Kellndorfer et al., global seasonal Sentinel-1 coherence and backscatter (checked earlier, re-read here)

- Registry entry:
  https://raw.githubusercontent.com/awslabs/open-data-registry/main/datasets/ebd-sentinel-1-global-coherence-backscatter.yaml,
  `License:` "The use of these data fall under the terms and conditions
  of the Creative Commons Attribution 4.0 International Public License.
  Contains modified Copernicus Sentinel data. -- Any use of this data set
  in publications or sharing should use this citation: "Global Seasonal
  Data Set of Interferometric Coherence and Backscatter from one Year of
  Sentinel-1 C-band SAR Data". Josef Kellndorfer, Oliver Cartus, Marco
  Lavalle, Christophe Magnard, Shadi Oveisgharan, Batu Osmanoglu, Paul
  Rosen, Urs Wegmüller. doi: In Review."
- The paper has since been published: Kellndorfer, J. et al. (2022),
  "Global seasonal Sentinel-1 interferometric coherence and backscatter
  data set", Scientific Data 9, 73, https://doi.org/10.1038/s41597-022-01189-6.
  The dataset DOI is https://doi.org/10.5067/8W33RRS6S2RV (ASF DAAC).
  Both come from a search result and were not read on the publisher's
  page. Check them before they go into a shipped notice.
- Credit line: **"Global seasonal Sentinel-1 coherence and backscatter,
  Kellndorfer et al. 2022, CC BY 4.0, modified; contains modified
  Copernicus Sentinel data 2019-2020"**.

## OpenStreetMap stays the only source for what OSM maps

The licences allow everything above. Lew's rule draws a different line,
and it falls on WOfS. A dry lake is something OSM maps
(`natural=water` + `intermittent=yes`, `water=lake`/`salt_lake`,
`natural=wetland`, salt flats). If WOfS paints a lake outline, it
competes with OSM's. So 1743 should use WOfS only as a **ground texture
under, or beside, what OSM does not map**: the frequency of wetness as a
surface class (a pan's crust or bed), never as a water body drawn over
OSM's polygons or in place of them. Where OSM has a water or wetland
polygon, OSM wins. Following the same rule, none of the radar or
temperature classes may draw roads, buildings, water or anything else
from OSM's feature set. They shade ground.

## Draft THIRD-PARTY.md rows (for 1743, not committed now)

These go into the "Data" table once 1743 commits the files. The paths
are placeholders until 1743 names them.

| Path | Origin | Licence | Role |
|---|---|---|---|
| `<ground packs>` (Africa ground classes) | Digital Earth Africa, Water Observations from Space (`wofs_ls_summary_alltime` / `_annual`), Landsat-derived; algorithms by N. Mueller (Geoscience Australia) and D. Roberts (ANU); resampled and classified by `<1743 tool>` | CC BY 4.0 (docs.digitalearthafrica.org/en/latest/data_specs/Landsat_WOfS_specs.html, read 2026-11-14); credit "Water Observations from Space © Digital Earth Africa, CC BY 4.0, modified" in the pack's directory piece and the credit screen | Pan and dry-lake-bed ground class where OSM maps no water; built into no binary, shipped as data packs |
| `<ground packs>` | Digital Earth Africa, Sentinel-1 RTC gamma-0 (`s1_rtc`), processed by Sinergise Sentinel Hub from ESA Sentinel-1 GRD; composited and classified by `<1743 tool>` | Copernicus Sentinel data terms, Regulation (EU) 377/2014 and Delegated Regulation (EU) 1159/2013 (Sentinel_Data_Legal_Notice, sha256 `fa2955ff...2fa64`); notice "Contains modified Copernicus Sentinel data [years]" | Surface roughness input of the ground classes; shipped as data packs |
| `<ground packs>` | JAXA, ALOS/ALOS-2 PALSAR/PALSAR-2 25 m annual mosaic (`alos_palsar_mosaic`), via Digital Earth Africa; composited and classified by `<1743 tool>` | JAXA Terms of Use of Research Data (earth.jaxa.jp/en/data/policy/, read 2026-11-14): use, modify and distribute free with credit; copyright JAXA; **commercial use only after notifying JAXA in advance**; credit "Credit: ALOS/ALOS-2 PALSAR/PALSAR-2 annual mosaic (JAXA), via Digital Earth Africa; modified" | L-band roughness input of the ground classes; shipped as data packs (only if Lew keeps it, section 3) |
| `<ground packs>` | USGS Landsat Collection 2 Level-2 surface temperature (`ls8_st`, `ls9_st`, ...), via Digital Earth Africa; DOIs 10.5066/P9OGBGM6, 10.5066/P9C7I13B, 10.5066/P9IAXOVV | Public domain, US federal data ("There are no restrictions on Landsat data downloaded from the USGS; it can be used or redistributed as desired", DE Africa Landsat_C2_ST_specs); credit "Landsat Collection 2 Level-2 surface temperature courtesy of the U.S. Geological Survey" | Thermal input of the ground classes; shipped as data packs |
| `<ground packs>` | Digital Earth Africa GeoMAD (`gm_s2_annual` / `gm_ls8_annual`); algorithms by D. Roberts (ANU) | CC BY 4.0 (GeoMAD_specs, read 2026-11-14); credit "GeoMAD © Digital Earth Africa, CC BY 4.0, modified", plus "Contains modified Copernicus Sentinel data [years]" for `gm_s2_*` | Surface colour input of the ground classes; shipped as data packs |
| `<ground packs>` | Kellndorfer et al. 2022, global seasonal Sentinel-1 coherence and backscatter (Earth Big Data, AWS Open Data / ASF DAAC 10.5067/8W33RRS6S2RV) | CC BY 4.0; "Contains modified Copernicus Sentinel data"; credit "Kellndorfer et al. 2022, Scientific Data 9:73, CC BY 4.0, modified" | Coherence input of the ground classes; shipped as data packs |

Whichever subset 1743 actually uses, the attribution must also reach
the user. Each pack's directory piece carries the credit lines, the way
the Copernicus DEM height packs carry their notice, and the map's credit
screen lists them. CC BY 4.0 Section 3(a)(1)(a)(v) asks for a URI "to
the extent reasonably practicable", so the credit screen names the URLs
above.
