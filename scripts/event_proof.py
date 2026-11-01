"""Mechanical input-fact matching for the live observation smoke check.

Epoch 2 adds a labeled place/magnitude pair to exact IDs. This checks reported
input facts, not comprehension, complete coverage, or model instruction obedience.
"""
import re

def reports_input_fact(text, observations, independent_fetch=False):
    if independent_fetch:
        return False
    sentences=re.split(r'[!?;\n]|\.(?!\d)',text.casefold())
    for row in observations:
        payload=row.get('payload',{})
        ident=payload.get('event_id')
        if ident and re.search(r'(?<![A-Za-z0-9])'+re.escape(ident)+r'(?![A-Za-z0-9])',text):
            return True
        place=payload.get('place');mag=payload.get('magnitude')
        if not isinstance(place,str) or not isinstance(mag,(int,float)):
            continue
        for sentence in sentences:
            if place.casefold() not in sentence:
                continue
            values=re.findall(r'\b(?:magnitude|mag\.?|m)\s*[:=*\s]*(-?\d+(?:\.\d+)?)',sentence)
            if any(abs(float(value)-mag)<1e-6 for value in values):
                return True
    return False

if __name__=='__main__':
    # Contract anchors are independent authored cases, not transcripts used to
    # tune the matcher. Swapped pairs guard against mere global keyword presence.
    rows=[{'payload':{'event_id':'aa123','place':'Alpha ridge','magnitude':2.3}},
          {'payload':{'event_id':'bb456','place':'Beta bay','magnitude':4.1}}]
    anchors=[('Report aa123 arrived.',True),
             ('An M2.3 earthquake occurred at Alpha ridge.',True),
             ('There were some earthquakes.',False),
             ('Magnitude 9.0 at Alpha ridge.',False),
             ('Magnitude 4.1 at Alpha ridge. Magnitude 2.3 at Beta bay.',False),
             ('Alpha ridge is 2.3 km away.',False)]
    for text,expected in anchors:
        assert reports_input_fact(text,rows)==expected,(text,expected)
    assert not reports_input_fact('Report aa123 arrived.',rows,independent_fetch=True)
    print('PASS input-fact matcher anchors: ID, exact pair, generic/wrong/swapped/units-only rejection, independent-fetch rejection')
